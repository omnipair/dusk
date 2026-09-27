# Volume and interest accounting

Index only successful, finalized transactions. Decode Dusk's event self-CPIs,
including those nested beneath the leverage delegate. Identify an event by
transaction signature, invocation path, and event ordinal; a transaction or
position can contain multiple executions. Match related records within their
originating instruction invocation, not by position alone.

## Swaps and swap fees

`SwapExecuted` is the canonical record of each actual Dusk AMM execution:
ordinary swaps, hLP rescue swaps, leverage open/increase/decrease/close/liquidation,
and the internal swap in a credit liquidation backstop. A backstop without a
swap emits no swap event. Adding or removing leverage margin also emits none.

`origin` identifies the initiating product/action. `trader` is the beneficial
owner, `actor` is the signer/executor, and `position` links leverage and credit
liquidation executions to their positions. `slot` records execution time.

Amounts have the same meaning for every origin:

- `amount_in`: AMM input before trading fees, after any input token transfer fee;
  includes borrowed principal when it funds a leverage swap.
- `amount_in_after_fee`: input applied to the pricing curve.
- `amount_out`: AMM output after trading fees, before output token transfer fees.
- `gross_amount_out`: output before an output-denominated trading fee.

These are pool execution amounts, not wallet deposits or withdrawals. This
changes the former standalone swap event's wallet-debit/net-wallet-credit
interpretation. Update consumers alongside the regenerated IDL.

Value one side of each execution, using the same price and fee convention for
all origins. DEX volume includes leverage-originated executions. Direct spot
volume can be filtered by origin. Leverage lifecycle events remain the source
for margin-position activity; their embedded swap receipts repeat the execution
for context and must not be counted again as volume or fees.

Swap trading fees are `base_fee + divergence_fee + volatility_fee`, in the mint
identified by `fee_asset_side`. `retained_fee` and `compounded_fee` describe
allocations of those fees, not additional fees. Filter by origin to distinguish
direct spot fees from margin-driven swap fees. Reporting both the total and its
margin subset is intentional overlap; do not add the subset to the total.

Future collateral paths can open a position with less swapping, multiple swaps,
or no swap. Emit exactly the executions performed. Position notional must stay
independent of swap input. The current opening path still swaps debt-asset margin
plus borrowing; this change does not implement additional collateral modes or
define gross position notional as an external venue's accepted trading volume.

For direct credit origination, use `MarketDebtUpdated.cash_debit` from `borrow`
invocations: this is principal leaving the pool before any transfer fee.
`cash_credit` is the borrower's net receipt. Keep repayments, liquidations,
and supplier deposits as separate flows. Outstanding debt is a balance, not
volume, and its changes include interest and share rounding. Margin-position
activity comes from leverage lifecycle events under an explicit notional
convention; it must not be inferred from the amount swapped. These product
metrics overlap and must not be added into an unlabeled total volume.

## Interest accrued

`BorrowInterestAccrued` records one asset's actual borrow-index checkpoint:

- `credit_interest`: interest on the fixed shares used by direct credit positions.
- `margin_interest`: interest on isolated leverage-position shares.
- `hlp_interest`: interest on aggregate hLP funding shares.

All three amounts use the same asset's borrow index. Each is the difference
between that bucket's independently floored debt at the old and new indexes,
with shares held constant during accrual. Do not split an aggregate interest
number proportionally afterward: that can misattribute rounding atoms.

The event carries the asset mint/side, old/new index, and `from_slot`/`to_slot`.
Accrual is lazy: a spot trade, liquidity action, governance action, or lending
action may checkpoint interest owed by all three sources. Attribute it to the
debt buckets, never to the instruction triggering the checkpoint. No event is
emitted for a zero-atom accrual or a repeated checkpoint in the same slot.
Read-only previews use quiet transitions and emit no accounting events.

These are recognized accruals at checkpoint time, not continuous daily marks.
There is no event while a market is idle, and the program's elapsed-time cap
still applies. A report that assigns amounts to economic calendar days must
state its interval policy; do not assume uniform daily accrual or add estimated
uncheckpointed interest to a later actual checkpoint twice.

Accrued interest is not collected cash, protocol revenue, or supplier yield.
Writeoffs and debt-share rounding adjustments are separate from accrual and
must be handled separately when reconciling outstanding receivables.

## Interest paid

`BorrowInterestPaid` is the canonical record of an interest payment, including
inline hLP settlement. `source` is `Credit`, `Margin`, or `Hlp`. A margin
repayment stays margin interest even when it occurs through adding margin;
hLP funding settled during a spot swap stays hLP interest.

- `interest_paid`: gross interest collected, excluding repaid principal.
- `interest_vault_credit`: actual net interest-vault receipt.
- `protocol_interest_revenue`: protocol allocation from that net receipt,
  before referral allocation.
- `referral_amount`: the portion of the protocol allocation owed to the referrer.
- `caller_bounty`: gross terminal hLP caller reward funded from interest.

The protocol's retained allocation is `protocol_interest_revenue -
referral_amount`; the supplier allocation is `interest_vault_credit -
protocol_interest_revenue`. Transfer fees and terminal hLP bounty explain why
gross collection can exceed vault receipt. Protocol-auction realization is a
later event and is not a second collection of borrower interest.

Lifecycle and referral events may repeat paid interest for context. Count the
canonical payment once; do not add `ReferralInterestAccrued`, `YieldClaimed`, or
embedded lifecycle interest to it. In particular, `ReferralInterestAccrued`
means a referral entitlement from a payment, not new borrower interest.

Accrual and payment events are separate reporting bases. Maintain both series;
never sum them into one fees/revenue series. Convert raw mint atoms to a common
currency off-chain with explicit pricing and reporting-period conventions.

## Borrow positions

Every instruction that writes a `BorrowPosition` reports the fields it can
change, as the position's state after the instruction:

| Instruction | Event | Position state carried |
|---|---|---|
| `deposit_collateral`, `donate_collateral` | `MarketCollateralDeposited` | `position`, `position_id`, `owner`, collateral, health contributions, liquidation CFs, `auction_debt_asset` |
| `withdraw_collateral` | `MarketCollateralWithdrawn` | `position`, collateral, health contributions, liquidation CFs, `closed` |
| `borrow`, `repay` | `MarketDebtUpdated` | `position`, fixed debt shares, health contributions, liquidation CFs, `auction_debt_asset` |
| `start_liquidation_auction` | `LiquidationAuctionStarted` | auction side, start time, start and floor prices |
| `fill_liquidation_auction`, `backstop_liquidation_auction` | `BorrowPositionLiquidated` | collateral, fixed debt shares, health contributions, liquidation CFs, `auction_debt_asset`, `closed` |
| `fill_liquidation_auction`, `backstop_liquidation_auction` on a recovered position | `LiquidationAuctionCancelled` | auction cleared |
| `withdraw_all_collateral` | `DebtFreePositionClosed` with `leverage = false` | position closed |

The first deposit creates the position with no debt, zero liquidation CFs, no
referral binding, and no auction. `auction_debt_asset` is `255` when no auction
is active; the auction start time and prices are then zero, and otherwise keep
the values from `LiquidationAuctionStarted`. Deposits and repayments that
restore health cancel an active auction, so their events carry the resulting
auction side. A side's referral binding is set by `ReferralBound` and clears
whenever that side's fixed debt shares reach zero. `MarketDebtUpdated`'s
`fixed_base_debt` and `fixed_quote_debt` are market-wide totals, not the
position's debt. A position whose `closed` flag is set, or whose
`DebtFreePositionClosed` has `leverage = false`, no longer exists. These fields
are appended after each event's earlier fields.

## Market observations

`MarketObserved` is emitted only by the permissionless `observe_market` crank.
It describes the committed market after the refresh every market instruction
runs before acting, so an idle market gains a price and yield point whenever a
keeper cranks it. Each side reports its asset mint and decimals, live reserve,
spot price, symmetric price EMA, swap-fee and interest growth indexes, and
borrow index. Prices quote the side's asset in the opposite asset and equal
`preview_market` for the same committed state and slot. `ylp_supply` is the
internal yLP share supply; `eligible_ylp` is the direct yLP counted by
governance sponsorship and queueing (live mint supply plus
`governance_locked_ylp`, minus yLP held by both hLP vaults).

The growth indexes are per-yLP-share accumulators scaled by 2^64: the index
difference between two observations, divided by 2^64, is the swap-fee or
interest yield that became claimable per yLP share over that interval. They
move when swap fees or paid interest become claimable by yLP holders. Interest
that has accrued but is still owed appears instead in the borrow index and in
the debt side's live reserve.

The crank publishes any non-zero `BorrowInterestAccrued` records for the same
checkpoint, exactly like other touches. A second observation in the same slot
changes no state, repeats the same values, and emits no accrual.

## LP transfers

`LpTransferred` is emitted by the Dusk Token-2022 transfer hook for every yLP
and hLP transfer that reaches it, after both holders' yield accounts are
checkpointed. It carries the market, LP mint, token kind (`0` yLP, `1` hLP),
source and destination owners, and amount. Holders are identified by owner
because the hook accepts only each owner's canonical LP token account.

The hook publishes it by self-CPI, like every other Dusk event, when the LP
mint's extra-account list includes the event authority and the Dusk program.
Mints initialized with the earlier seven-entry list still transfer normally
but publish no receipt; track those balances another way.

Transfers to the same account never reach the hook and report nothing; a
zero-amount transfer reports amount `0`. Combine this event with the
liquidity, hLP, and governance events for LP supply changes made through Dusk.
A direct Token-2022 burn bypasses the hook and emits no Dusk event.

## Interface changes

`initialize_yield_accounts` and `start_liquidation_auction` now require Anchor's
event authority and program accounts so their real accruals can be emitted by
self-CPI. SDK instruction construction resolves those accounts from the new
IDL. `LeveragePositionUpdated` also exposes `interest_paid`, including decreases,
partial closes, and margin repayments. Persistent account layouts, borrow
rates, repayment allocation, and lending economics are unchanged.
