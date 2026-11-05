# @omnipair/dusk-sdk

Current loss and fee specification: [junior-first credit loss waterfall](../../docs/CREDIT_LOSS_WATERFALL.md).

For volume, fee, and borrower-interest consumers, see the
[accounting event contract](../../programs/dusk/ACCOUNTING_EVENTS.md).
`SwapExecuted` covers spot and leverage AMM executions and the market state they
leave behind. `BorrowInterestAccrued`
separates credit, margin, and hLP accrual; `BorrowInterestPaid` reports actual
collections with the same source attribution. Typed events and their
`SwapOrigin`/`DebtSource` discriminants are exported from this package.

TypeScript SDK for Dusk, the Omnipair V2 protocol architecture. This package
targets Dusk market layout 11, including bounded oracle policies, aggregate leverage exposure and stored
position margin terms. Regenerate clients and use matching account layouts.

## Bounded oracle markets

Market creation selects native or oracle-guided pricing in the same
`initializeMarket` instruction. In the SDK, omit `oracle` (or pass `null`) for
native pricing at λ = 0; pass an `OraclePolicyInput` to launch with that policy:

```typescript
const instruction = await dusk.write.initializeMarketInstruction({
  ...creationParams,
  oracle: policy, // omit or use null for native pricing
});
```

The creator does not send a separate oracle-configuration instruction. Creation
validates and stores the policy atomically with the market and its LP metadata;
`MarketCreated.oracle` reports the selected policy. Creation needs no live feed
account. First liquidity and subsequent price-sensitive operations authenticate
a current report when λ is positive. An invalid initial policy rolls back market
creation and the treasury creation charge.

Guided creation has no independent opening price: omit `bootstrapPriceNad` or
pass zero. The SDK and program reject a nonzero value when λ is positive.
`addLiquidityInstruction` takes two maximum gross spends. On the first guided
deposit the validated execution-time report sets their usable ratio; base is
rounded down and quote up to token atoms, and unused tokens stay in the owner's
wallet. The existing `minSrlpAmount` floor still applies. Native first deposits
keep their caller-selected ratio, and subsequent deposits remain proportional
to existing reserves. The first-deposit change preserves all wire layouts.

Every market can later activate an oracle, change its source/weight/cap, or return
to native pricing through the existing parameter-proposal lifecycle.
`parameterUpdate.oracle(policy)` creates the proposal update. Subsequent changes
are creation-only, including before first liquidity; no post-creation setter
remains. `oraclePolicy()` validates the wire format. Positive weights are
1–9,999 bps and a nonempty policy's adjustment multiplier must exceed one;
there are no creation-time oracle ceilings or automatic outage-driven changes.
See [the implementation contract](../../docs/OP_GAMM_IMPLEMENTATION_STATUS.md)
for the coupled swap, lending and hLP behavior.

Use `marketCreationLookupTablePlan` and `marketCreationV0Transaction` for the
creation transaction. The latter rejects a packet larger than 1,232 bytes;
metadata and optional setup instructions must fit alongside the selected policy.
An address lookup table compresses account addresses, not metadata or policy
bytes. The launch scripts accept optional `DUSK_ORACLE_POLICY_JSON` using
`OraclePolicyInput` field names (base58 addresses, 32-byte feed-ID arrays and
string values for large integers); omission selects native pricing. They resolve
the stored policy's feeds and compute budget when seeding liquidity.

Raw Anchor callers must include `oracle: null` or a serialized policy. The
`initializeMarket` argument layout now starts with `paramsHash`, followed by
`config` and `oracle`; rebuild instruction encoders and event decoders using the
vendored IDL. The removed `configureInitialOracle` instruction and
`InitialOracleConfigured` event are replaced by creation parameters and
`MarketCreated.oracle`.

Provider and feed layout are independent: normalized direct pair
(`NORMALIZED_ORACLE_V1`, format 1), fully verified Pyth ratio (`PYTH_RATIO_V2`, 2),
fully verified Pyth direct pair (`PYTH_DIRECT_V3`, 3), or normalized ratio
(`NORMALIZED_RATIO_V4`, 4). One feed must actually quote the ordered market pair;
META/USD alone is not META/USDC. Two feeds must share a denomination and include
an observed quote-asset price. Ratio uncertainty is rounded outward and each leg
is authenticated and checked for age and publication skew. Layout 8 stores
per-leg counters/content hashes so independent updates cannot hide normalized
report equivocation or rollback. Do not decode layout-7 accounts with this SDK.
A market selects the provider, feed identities, age and confidence policy. The normalized provider
must serialize the exact Borsh `NormalizedOracleV1` envelope defined in
[oracle.rs](../../programs/dusk/src/oracle.rs), including the underlying source
observation time, both mints and a monotonically increasing nonzero sequence.
Dusk does not fetch an off-chain report or give Horus special authority.

High-level builders fetch the policy and append feed accounts after the existing
hLP/hook prefix. Positive-weight transaction builders add the compute and heap
budgets. When composing **raw instructions**, add
`oracleComputeBudgetInstructions(policy)` once before constructing any
index-bound flash plan; do not prepend instructions after binding its indices.
Resolve `oracleReadAccounts(policy)` for direct Anchor calls. Zero weight needs
neither feed accounts nor OP budgets. Simulate the whole transaction, including
any provider update and callback: a passing isolated price calculation does not
establish route feasibility.

A stale or invalid source rejects price-sensitive operations. Direct repayment
in the debt token and adding collateral remain possible without a valid price;
this does not permit a collateral sale or removal. Oracle updates change the
market reference but do not execute an hLP hedge by themselves.

## Protocol revenue defaults

`DEFAULT_PROTOCOL_REVENUE_SHARE_BPS` is exported from the generated program IDL.
Its initial value is 2,000 bps (20%). It initializes the protocol share of base
swap fees and the separate additive borrower interest markup, and sets the
protocol share of liquidation fees. Dynamic swap
surcharges remain excluded. Liquidation's total charge is fixed at 3% of net debt
repaid, subject to available surplus; the shared constant sets the protocol's
share of that charge.

`initFutarchyAuthority` now accepts nullable `swapBps` and `interestBps`. Pass
`null` for the program default, or a number for an explicit override, including
zero. The FutarchyAuthority signer can subsequently change swap share or interest markup and the auction split.
The bootstrap script uses the program defaults when `DUSK_PROTOCOL_SWAP_BPS` and
`DUSK_PROTOCOL_INTEREST_BPS` are unset. Regenerate clients for this instruction change.
Changing the Rust default affects new authorities and liquidation allocations;
existing authorities retain their configured swap share and interest markup.

The initial auction split remains 100% fee auctions / 0% buybacks. Referral
commissions come out of the protocol interest share before the auction split.
The separate voluntary-order service charge and flat market-creation charge
are not revenue-share percentages and are outside this default.

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

`get.simulateWithContext(plan.instructions, { requireReturnData: false, lookupTables })`
preserves the absolute begin/settle indices in a built flash plan. Existing
compute budgets are retained (or explicitly overridden in place). A flash plan
missing either budget is rejected rather than silently shifted. Ordinary
previews receive a budget prefix. Set `preserveInstructionIndices: true` for
other programs with index-bound plans. Do not prepend instructions after `buildFlash`; pass
setup instructions through its `prefixInstructions` argument instead.

The auction start/fill/backstop and old full leverage-liquidation instructions
have been replaced. Use `dusk.liquidations` for either borrowing or leverage:

```typescript
const position = { market, position: positionAddress, kind: "leverage" as const,
  debtAsset: "quote" as const };
// Read-only simulation; no registration transaction is required.
const health = await dusk.liquidations.previewHealth(position);
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

Emergency internal settlement uses **symmetric EMA price and smoothed curve depth**,
with the market's concentration. Both preview and settlement report
`referencePriceNad` (debt per collateral at 1e9 scale) and `referenceDepthNad` (normalized
curve depth, not TVL). `buildEmergency()` also returns these as bigint
`reference.priceNad` / `reference.depthNad`. `swapOutput` is the debt value credited
by this inventory settlement; it is not a live-spot execution quote. The pool
receives real collateral and reduces the loan, with funded cash payouts and real
hLP reconciliation. Normal flash routes, including `settlement: "duskAmm"`, retain
their venue's executable quotes.

The full fee schedule remains: launch price and divergence use the reference
curve; volatility uses the existing accumulator and decay. Thus changed fee
history can change net proceeds even when price/depth EMAs are unchanged. Live
spot/depth are not substituted into reference sizing or its full-close proof.
Use a fresh preview and simulate the complete transaction. The returned reference
is not a lock; the builder's minimum-output/reward arguments still apply.

They enforce the emergency EMA-health gate (equity at or below 70% of MM).
A full reference-priced settlement needs nonpositive EMA equity or the program's conservative proof
that no smaller reference-priced settlement can restore MM +2 percentage points after all costs.
A useful below-target partial or an external flash opportunity does not veto
this fallback. Full normal swap fees, rewards and loss accounting still apply.
Failure to certify, including proof-budget exhaustion, rejects full closure.
Smaller improving fills remain available. Preview and simulate the intended
operation: a failed slice is not full-close permission. There is no configurable
minimum remaining debt.

Ordinary `preview({ full: true })` / `buildFlash({ full: true })` retain the
stricter no-improving-partial certificate. An accepted full `previewEmergency`
quote does not authorize a full ordinary flash fill. Both builders use their
respective on-chain previews; do not infer permission from the incentive helper.

Builders return unsigned instructions and owner-owned payout accounts. Prefix
instructions belong in the builder request: inserting instructions afterwards
invalidates the bound settle index. Helpers include ATA setup, a 256 KiB heap,
a 1.4M compute-unit limit and required hLP accounts. A compute-unit limit supplied
in the prefix overrides the default; set the priority price there as needed.
The session closes at settle. Its payment token account persists for reuse,
including withheld Token-2022 fees; preexisting token donations do not count as
payment and are not refunded to the next buyer.

The 0.5%–5% ramp sets a maximum buyer discount. Partial quotes choose the highest
whole-basis-point discount within that range that improves health after the
3% repayment charge and collateral transfer costs. If none works, the partial
rejects. Read the actual rate from `quote.rates.buyerDiscountBps`; use
`liquidationIncentives(...).maximumBuyerDiscountBps` only for the ceiling. The
legacy helper field `buyerDiscountBps` remains a deprecated alias for that ceiling.
Position-health previews also report the ceiling, not an executable purchase.

Market creation has no minimum remaining-debt setting. Partial `maxRepayment`
requests retain at least one debt share; small remainders are allowed when the
fill improves health. A request beyond the first MM+2 recovery crossing rejects;
request a smaller amount. Emergency collateral slices also respect health
improvement and a conservative recovery cap. The bound includes base swap fees and
checks surviving AMM amounts against actual execution costs. A bounded search
can still refuse a target crossing when it cannot certify the result;
useful below-target slices remain permitted. Preview acceptance does not reserve
liquidity or guarantee keeper profitability. Simulate the complete transaction.

`liquidationHealth`, `liquidationIncentives`, `liquidationFeeAllocation`,
and `liquidationLossAllocation` use integer atoms
and mirror the policy arithmetic. They do not replace the on-chain execution
preview. Incentives depend only on current health; there is no time ramp or
stored distress episode. `liquidationIncentives` takes only the health input.

Indexers should consume `FlashLiquidationBegun`, `FlashLiquidationSettled`,
`EmergencyLiquidationSettled`. AMM settlement also emits
`SwapExecuted`; external purchases do not generate Dusk swap volume. For emergency
settlement, its start price/output describe the reference-priced debt offset;
its final reserve/spot snapshots describe the actual pool. Correlate it with
`EmergencyLiquidationSettled` rather than treating it as a live execution quote. Collected
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
five-account prefix exactly once: `[srLP mint, base hLP srLP vault, quote hLP srLP
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

### Position actions

See [the position action contract](../../docs/LEVERAGE_POSITION_ACTIONS.md) for
amount units, rounding and the six agreed controls. All sizing is in the debt
token on both market directions; neither token needs to be a stablecoin.

- `increaseLeverageInstruction`: borrow and buy, with no wallet funding.
- `addToLeverageInstruction`: swap `marginAmount` (gross debt-token funding) plus
  `debtAmount` into held collateral. Select leverage for the whole resulting
  position; `planLeverageIncrease` defaults to its current leverage.
- `addLeverageMarginInstruction` / `repayLeverageInstruction`: repay debt without
  swapping held collateral. No standalone margin withdrawal is supported.
- `decreaseLeverageInstruction`: sell collateral into repayment, including full
  repayment to 1x. `maxSurplusOut` defaults to zero and explicitly bounds any
  gross surplus returned to the owner's debt-token account.
- `closeLeverageInstruction`: `closeBps` selects a proportional owner partial
  close; omit it for a full close. `minAmountOut` is net wallet receipt.

`leverageActionMetrics`, `planLeverageIncrease`, `planLeverageDecrease`,
`planLeverageRepayAll`, `leverageRepaymentQuote`, `leverageCloseSlice` and
`leverageClosePercentage` support linked controls with bigint arithmetic.
Sizing callbacks must use same-snapshot, fee-adjusted internal quotes. For a
final executable check, simulate the actual action:

```ts
const ix = await dusk.write.addToLeverageInstruction({
  ...positionAccounts,
  ownerDebtAccount,
  marginAmount: 100_000_000n,
  debtAmount: 200_000_000n,
  minCollateralOut,
});
const preview = await dusk.get.previewLeverageAction({
  market, leveragePosition, instructions: [ix], feePayer: owner,
});
// Review preview.walletFlows, preview.metrics, and preview.marginTerms.
// Simulation never submits or signs the transaction.
```

Metrics returned by this preview use the symmetric EMA, before any hypothetical
liquidation costs. They are not executable close payout or liquidation eligibility;
the actual action's wallet flows come from its simulated settlement events.

### Swap and LP Transfer Events

`SwapExecuted` records the market immediately after a completed trade: the
internal srLP supply and, per side, a `MarketSideSnapshot` with the spot price
and price EMA exactly as `preview_market` reports them for the same state and
slot, plus the swap-fee and interest growth indexes. `start_price_nad` is the
Base price actually used to quote that trade, after any pending controller
step. Swap events provide historical execution and post-swap observations;
they are not a continuous live-price feed. For a prospective quote, use
`preview_swap` with the intended input and current state rather than treating
the last event's post-swap spot as the next trade's starting price.

`LpTransferred` reports each srLP or hLP transfer through the Dusk transfer
hook as an ordinary CPI event. `buildLpTransferHookAccountMetas` includes the
event authority the hook needs; Token-2022 ignores it for mints initialized
with the earlier seven-entry list, which transfer without a receipt.
`buildLpTransferHookValidationAccountData({ ..., legacyLayout: true })`
encodes that earlier list.

### Market operator

`dusk.operator` builds transactions without signing or submitting them. The
operator starts as the deployer. LP balances confer no authority in Dusk.
An external governance program may own the role using a signer PDA; its voting
and token economics remain outside the core program.

```typescript
import { feeParameterUpdate, createProposalMetadata } from "@omnipair/dusk-sdk";
const metadata = await createProposalMetadata({
  title: "Update swap fees", markdown, descriptionUri: "ipfs://pinned-description",
});
const announcement = await dusk.operator.announce({
  market, operator, payer, metadata,
  updates: [feeParameterUpdate(nextFeeProfile)],
});
// operator signs; a separate funded payer pays account rent.
const pending = await dusk.operator.proposal(announcement.proposal);
const verified = await dusk.operator.verifyDigest(pending);
// After pending.executeAfterSlot, anyone may execute within the window.
const execution = await dusk.operator.execute({market, proposal: announcement.proposal});
const cancellation = await dusk.operator.cancel({market, proposal: announcement.proposal, operator});
```

The announcement waits **4,752,000 scheduled slots** (maximum junior notice plus
one protocol day), then has a seven-day scheduled-slot execution window. There
is one current bundle per market. Replacing it restarts the full notice; stale,
executed or expired accounts may be closed to their original payer with
`dusk.operator.resolve`. Pending changes never lock LP receipts or exits.

To transfer authority, announce `nextOperator`; execution also requires
`acceptingOperator` to sign. A governance program accepts through CPI using its
PDA. An explicit all-zero next authority permanently renounces control; there
is no LP election or automatic deployer fallback. The monotonically increasing
nonce prevents reusing a closed announcement address.

Use `feeParameterUpdate`, `concentrationParameterUpdate`, `irmParameterUpdate`,
`emaHalfLivesParameterUpdate`, `dailyBorrowLimitParameterUpdate`,
`centerControllerParameterUpdate` and `trancheParameterUpdate` for the seven
mutable families. Updates are canonicalized and execute atomically. The oracle
policy and initial APR cannot appear in an operator update. APR has a fixed 1%
floor, no selectable ceiling, and a 12-hour–30-day half-life. The centering update
contains only the adaptive minimum step; concentration always retains the
funded EMA controller.

A concentration update includes `rampDurationSlots` (default/minimum 432,000).
Positive targets obey the 0.1×–10× rule; zero-width transitions still require the
announced guarded ramp. Swaps lazily attempt intermediate geometry. A rejected
step leaves the existing curve usable and does not prevent repayment or exits.

`createProposalMetadata` / `uploadProposalMetadata` verify exact UTF-8 Markdown,
length and SHA-256 before building metadata. `tryFetchProposalDescription`
returns verified content or a warning. Render verified Markdown with sanitized
GitHub-flavored Markdown and raw HTML disabled. Description availability never
controls execution. `verifyDigest` reproduces the on-chain Borsh commitment
using the generated account layout.

Operator interest revenue is 0–50% of base interest, set at creation or changed
by a timed bundle. There is no operator swap revenue. The separate global
FutarchyAuthority markup is added on top of base interest. Each market closes
the old interval before adopting the latest global setting at its next checkpoint.
The stored `debt.*RateAtTargetNad` is base APR. Apply the market's cached
`protocolInterestMarkupBps` when displaying total borrower APR; the borrow
index already includes that markup, so do not add it again to indexed debt.
For $100 base interest, 30% operator share and 20% markup: LPs receive $70,
the operator $30 and the protocol $20; the borrower pays $120.

```ts
const claim = await dusk.operator.claim({market, assetMint, payer});
// For an earlier recipient, pass its sealed revenueEpoch.
```

Fees are earned on collection. A handoff preserves already collected cash for
the previous recipient; later payments belong to the new one. `execute` returns
`setupInstructions` for a recipient archive when needed. Its `transaction`
already includes them; PDA forwarding must prepend them before forwarding
`instruction`. Neither claims nor swaps call the external governance program.

Virtual-reserve fields and signed debt/collateral event deltas use 128-bit
values. Keep decoded balances as `BN` or `bigint`; token transfer amounts still
use the unsigned 64-bit SPL domain. Extreme accrual saturates coherently across
debt and receivable books and advances its checkpoint without deferring excess
interest. This introduces no configurable APR cap.

See [operator policy and protection details](../../docs/MARKET_OPERATOR.md).

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
require their addresses to end in `srLP` (the srLP mint) and `hLP` (both hLP
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
const srlp = await createHookedLpMintWithSeedInstructions({
  payer: creator, base: creator, seed: seeds.srlp.seed, mint: seeds.srlp.mint,
  decimals, mintAuthority: market, transferHookProgramId: PROGRAM_ID, lamports: rent,
});

const naming = marketLpTokenNaming({ baseSymbol: "META", quoteSymbol: "USDC" });
// naming.srlp      -> { name: "META/USDC srLP", symbol: "srME/US", description }
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

### Mutable transfer-fee notice monitoring

```ts
import { DuskLiquidations, transferFeeCredit } from "@omnipair/dusk-sdk";
const liquidations = new DuskLiquidations(program, keeperPublicKey);
const notice = await liquidations.transferFeeNotices({
  market, position, kind: "leverage", debtAsset: "quote",
});
// Each mint and Clock were read from the same RPC bank.
console.log(notice.slot, notice.epoch, notice.collateral.phase);
const actualReceipt = transferFeeCredit(grossCollateral, notice.collateral, "effective");
const healthCredit = transferFeeCredit(grossCollateral, notice.collateral, "liquidation");
```

`fetchTransferFeeNotices(connection, mints, { minContextSlot })` also supports
1–99 mints per coherent request. Monitor both debt and collateral mint
`refreshKey`s and poll epochs even when mint data does not change. `nextRecheckEpoch`
is the next notice/activation boundary, not an instruction to wait until then.
A change or cancellation invalidates prior health/payment estimates. Refresh the
on-chain flash/emergency preview and simulate the complete transaction before
signing; these helpers do not prove eligibility or promise a fill. Use effective
fees for actual transfers, never a pending fee as a current payment fee.

See [the fee policy and keeper rollout dependency](../../docs/mutable-transfer-fees.md).
#287160 has implemented notice-window mitigation; timely confirmed keeper execution
remains an operational assumption. Fee-authority revocation is not a missing
agreed fix.

### hLP funding capacity

Use `dusk.get.previewHlpDepositCapacity({ market, baseMint, quoteMint, targetHlpMint })`
to read the native entry status and gross/net funding ceilings. It accrues the
market in a read-only simulation and prices the underlying srLP purchase and
hedge using full live-reserve value, including public unpaid interest. These
receivables remain excluded from swap-pricing reserves.

`Ready` and the capacity ceiling do not guarantee execution for every amount.
Simulate the completed deposit transaction to check rounding, cash custody,
transfer fees and slippage. Under concentration, an opposite-token-neutral
hLP need not have a literal 2x gross-value/equity ratio.

### Public-interest cash advance calculations

`quoteInterestAdvanceAmounts` computes the cash-capped gross advance, frozen
protocol escrow, LP transfer debit and measured-fee-adjusted LP credit. Pass
amounts as `bigint` and the current `TransferFeeNotice`; pending transfer fees
are reported separately by that notice and do not replace the effective fee.
`allocateRepaidInterest` splits measured repayment custody credit between
fresh LP/protocol revenue and the already-frozen protocol portion. Already
funded LP interest must be excluded from `freshGrossInterest`.

For transaction composition, use the vault-aware yield client:

```ts
const quote = await dusk.yield.previewFunding({ market, caller });
const plan = await dusk.yield.buildHarvest({
  market, owner, caller, kind: "baseHlp", asset: "quote",
  // Optional per-asset gross advance caps, in raw atoms:
  maxBaseGross: 0n, maxQuoteGross: 1_000_000n,
});
// Simulate the complete plan.transaction before signing and sending.
```

`kind` accepts `srlp`, `baseHlp` and `quoteHlp`. The builder reads the owner's
YieldAccount, respects its configured recipient and creates that recipient's
ATA if needed. Existing yield accounts are required. Builders never sign or send.
Set `fundAccruedInterest: false` to harvest existing funded claims without a new
funding/rebalance attempt. `fundInstruction` is the standalone funding builder.

Funding moves only available cash. Newly earned hLP income first covers the
vault-wide funding gap; existing funded holder claims are protected. The hedge
reconstruction can change the AMM mark; the cash advance alone leaves swap
reserves unchanged. `HlpInterestAllocated` distinguishes new holder yield from
capital returned to reserves. Subtract its capital debits from gross
`BorrowInterestPaid` credits when reconciling net interest-vault changes.

Repayment builders supply the canonical accounts needed for both hLP vaults.
Raw callers must include the five existing hLP accounts followed by both asset
mints and reserve vaults when fresh public interest is settled with active hLP.
The Market layout is version 11 with `fees.publicInterestAdvance` on both sides,
per-vault flash locks, and non-cash `reserves.flashReceivable`;
use the regenerated IDL. It also records a frozen hLP mask and per-vault
curve offsets; older snapshots are incompatible.

### Vault-wide hLP flash recovery

For a solvent vault, this is a **flash rescue** of its funding hedge. A positive
funding gap can be quoted before the 112.5% critical-stress marker. Ordinary
swaps can also earn a recovery bonus below that marker. The on-chain flash
quote caps the discount to retain positive vault value;
it may offer no discount for a tiny gap. A frozen vault can also quote a funded
rescue after its gap falls. Full flash closeout remains restricted to insolvency
at the current-slot symmetric EMA; the caller's fixed payment determines actual
flash recovery.

```ts
const quote = await dusk.hlpLiquidations.preview({ market, targetAsset: "base" });
const plan = await dusk.hlpLiquidations.buildFlash({
  market, targetAsset: "base", buyer,
  allowFull: false, // explicit opt-in is required for an insolvent full closure
  route: async ({ buyerCollateralAccount, repaymentVault, requiredPaymentCredit,
                  collateralMint, debtMint, collateralTokenProgram, debtTokenProgram }) => {
    // Return an external route, a Dusk swap, or explicit inventory payment.
    // Deliver requiredPaymentCredit NET atoms into repaymentVault; gross up
    // its incoming transfer fee. The quote already includes vault→reserve fees.
    return buildRecoveryRoute({ buyerCollateralAccount, repaymentVault,
      requiredPaymentCredit, collateralMint, debtMint, collateralTokenProgram, debtTokenProgram });
  },
});
// Compile plan.instructions with a prepared address lookup table when needed.
// Do not insert/reorder instructions: begin binds the exact settle index.
```

The helpers never sign, submit, or silently top up from the keeper's wallet.
The quote is current-bank only. `maxPayment`, collateral limits and
principal-loss caps protect the submitted plan against state drift. Prefix
instructions must be supplied through `prefixInstructions` before building.
An affected hLP cohort cannot mint/redeem between begin and settle; swaps and
independent market operations remain composable subject to cash and accounting
checks. The SDK's complete-plan simulator preserves bound instruction indices.
`HlpFlashSettled.loss` separates principal recovery,
principal writeoff and canceled interest; a partial recovery has an empty loss
allocation. Native `HlpTerminalLiquidated.socializedLoss` now denotes principal
loss only, with `interestCanceled` reported separately.

If an hLP vault has negative executable value, ordinary spot swaps continue.
Only that vault's automatic hedge is frozen; the opposite vault remains active.
`state.amm.frozenHlpMask` uses bit 1 for base hLP and bit 2 for quote hLP.
The frozen vault cannot make an ordinary withdrawal that leaves its deficit to
the other holders. A terminal close requires insolvency at the current-slot
symmetric EMA, so a spot-only price move cannot write off the vault. Native
recovery is then valued on the EMA-price, smoothed-depth concentrated reference
curve with the configured fees; it does not use live spot reserves.

An eligible holder can build `hlpLiquidations.buildNativeCloseAndExit({ market,
targetAsset, exit: { hlpAmount, options } })`. The helper places the permissionless
native close before `withdrawSingleSided` in one transaction; `options` are the
normal hLP withdrawal accounts and the owner signs. It burns the closed vault's
receipts for zero principal payout. The flash builder also accepts `exit`,
appending an owner-signed withdrawal after settlement. For a solvent rescue,
set `exit.minTargetAmountOut` to protect the holder's payout. A terminal quote
still requires `allowFull` and pays zero principal. A vault with only a
transient spot deficit must recover or receive funded rescue before its holders
can withdraw. Once its executable value recovers, a frozen vault can quote a
funded flash rescue even if its debt gap has fallen below the ordinary rescue
threshold; a fully covered gap uses a minimal zero-discount payment.

Spot swap previews use the same cash policy as execution. If paying all accrued
hLP funding interest would consume the cash needed for trader output or the
last reserve atom, Dusk requotes the swap while carrying both active hLP
funding-interest amounts on their indexed debts. That swap pays no hLP funding
interest and offers no hLP recovery bonus. A later swap with sufficient reserve
cash can settle the carried interest. The trader's output still requires
spendable reserve cash; this fallback does not create liquidity.

### Restarting after a terminal hLP close

When a terminal close leaves old hLP receipts outstanding, a new deposit needs
a fresh receipt mint. Create a zero-supply hooked hLP mint with the market as
mint authority, then call `write.rotateHlpCohortInstruction` with the old and
new mints, target side and Metaplex metadata. The old mint's transfer hook must
be initialized first; `write.initializeLpTransferHookInstruction` accepts
`tokenKind: "hlp"` when composing that setup without an RPC market fetch.
Initialize the new mint's hook, then deposit using the new mint. The other hLP
side and srLP mint do not rotate.

The old mint remains transferable but owns no new principal. Its holder yield
indexes are frozen in `deriveHlpCohortAddress(market, oldMint)`. A holder can
initialize an old mint's yield accounts with
`write.initializeYieldAccountsInstruction({ ..., retired: true })`, claim
funded yield through `yield.buildHarvest({ ..., retiredHlpMint: oldMint })`, and
burn old receipts with `write.burnRetiredHlpReceiptInstruction`. If the asset
has a Token-2022 transfer hook, pass its extra accounts as
`transferHookAccounts` to the harvest builder. Burning never forfeits yield
already checkpointed in the holder's yield accounts.

## srLP, jrLP and hLP

`srLP` is the default senior claim. `jrLP` owns productive first-loss capital;
entering junior burns srLP and mints jrLP at their current values. `hLP` remains
a hedged position backed by senior liquidity and cannot enter junior. Its own
unpaid income remains in NAV, and collected income pays funding costs first.

```ts
const setup = await dusk.tranches.initialize({ market, payer });
// Sign and submit setup.transaction once for this junior generation.
const enter = await dusk.tranches.convert({
  market, owner,
  action: { kind: "enter", seniorAmount: 1_000_000n, minimumJunior: 990_000n },
});
// Sign and submit enter.transaction. Amounts are raw token atoms.
const request = await dusk.tranches.convert({
  market, owner, action: { kind: "requestExit", additionalJunior: 500_000n },
});
```

Exit requests escrow receipts. They continue earning junior income and taking
losses during notice and the exercise window. After notice, `convertExit` burns
the escrowed jrLP and mints srLP at its current value. It does not withdraw cash.
Read the returned `exit` account for its actual `readyAt` and `expiresAt` slots;
the configured crowding rule determines the notice. An expired request must be
renewed or canceled. Conversion can also be blocked by an unsettled insolvent
liquidation. A failed transaction does not silently extend a window.

Income earned before an ordinary LP transfers or converts remains with that
holder. Each asset has separate public-loan and hLP-funding income histories.
The SDK exposes bounded maintenance transactions; none are submitted implicitly:

1. Before issuing income into a fully collected book, call
   `prepareDrainedIncomeBooks({ market, payer })` and submit its transactions.
   For `IncomeEpochNeedsRollover` on a partially paid book, use `rollIncome` for
   the affected book. The payer funds its immutable archive account.
2. For an existing srLP/jrLP holder, call
   `prepareHolderIncome({ market, owner, kind: "senior" | "junior" })`.
   Submit its transactions and repeat while `hasMore` is true. Each instruction
   processes at most eight archived epochs for one source.
3. Fetch again and build the transfer, conversion or harvest. A competing
   maintenance transaction can require another catch-up; builders do not
   guarantee that the market stays unchanged between fetch and submission.

Archived history has no fixed lifetime limit. A caller that pays archival rent
can roll after settlement, so repayment/roll churn can increase other holders'
catch-up work. Eight epochs per instruction bounds transaction work, not total
history. Integrators should maintain active holders, retry stale plans and expose
maintenance transactions in their UX; this mechanism does not guarantee a
single-transaction exit for an inactive holder.

`checkpointIncome` also supports one asset/source batch and retired junior
mints. Wiped receipts have no principal but may still own historical earnings.
`burnRetired` removes those receipts without giving them new-generation capital.

The permanent junior seed receives income even without external jrLP holders.
`fundSeed` moves its collected interest to reserve custody. `compoundSeed`
reinvests balanced asset amounts without issuing senior tokens; unmatched assets
remain seed-owned custody until a matching amount is available. These custody
balances do not count as productive junior coverage until reinvested.

Rate defaults are exported as `RISK_RATE_DEFAULTS`; the market configuration and
creation helpers can override initial terms; the operator can tune future dynamics after notice. `riskCoverageSignalNad` uses one junior
capital budget across both debt assets, with each asset's own utilization.
