# Leverage position actions

Agreed on 2026-10-11 for PR #45. This describes the program/SDK contract; the
application UI and indexer must adopt the regenerated interface before rollout.

## Units and product behavior

Each position holds one asset and owes the opposite asset. Entry swaps the
owner's debt-token contribution plus borrowed debt into the held asset. There
is no optional funding mode or assumption that either token is a stablecoin.

Leverage is held-asset value / equity, with both values measured in the borrowed
token. A META-funded position holding USDC is long USDC against META. Its
asset-relative leverage differs from the dollar delta of a perpetual short.
Repaying META leaves USDC quantity unchanged and reduces this asset-relative
leverage. Do not silently switch numeraires for reverse-direction positions.

IM/MM remains the stored size-tier and crowding policy. Selecting a lower
leverage changes funding amounts, not the maintenance policy. Existing
`margin_amount`, `open_notional`, and `multiplier_bps` are opening metadata,
not current equity, current size, or current leverage. Recompute current
metrics from collateral, indexed debt and an explicitly labeled price basis.

| UI action | Program behavior | Wallet flow | SDK builder |
| --- | --- | --- | --- |
| Increase size | Borrow debt and buy more held tokens | None | `increaseLeverageInstruction` |
| Decrease size | Sell held tokens and use proceeds to repay debt | Only explicitly bounded unused output | `decreaseLeverageInstruction` |
| Increase margin | Repay debt; held-token quantity stays unchanged | Debt token into the reserve | `addLeverageMarginInstruction` / `repayLeverageInstruction` |
| Add to position | Swap new wallet funding plus optional additional borrowing | Debt token into the reserve | `addToLeverageInstruction` |
| Remove from position | Sell a proportional collateral slice and repay its proportional debt | Net residual in the debt token to the owner | `closeLeverageInstruction` with `closeBps` |

Standalone decrease-margin / borrow-to-wallet is removed from the program and
SDK. Other wallet funding/payout assets can be zapped by the application before
or after these operations, with their own explicit slippage limits.

## Controls and previews

- Increase size: link additional held-token size and target whole-position
  leverage. `planLeverageIncrease` translates either input into borrowing.
- Add to position: contribution amount plus target **whole-position** leverage,
  defaulting to the position's current leverage. A 3x position with 600 value,
  400 debt and 200 equity, adding 100 equity at 4x, needs 500 additional borrowing
  before costs. Targeting the new slice alone would produce the wrong result.
- Decrease size: link the sale amount with resulting leverage using
  `planLeverageDecrease`. It retains equity except execution costs and any
  expressly permitted unneeded repayment output.
- Remove from position: link percentage and held-token amount using
  `leverageClosePercentage` / `leverageCloseSlice`. Show expected wallet payout
  separately. `closeBps = 10000` closes everything; partial closes round collateral
  down and debt shares up, so share rounding cannot increase remaining leverage.
- The same market price applied before/after a proportional close gives the same
  leverage. Actual execution can change the price and payout. Remaining debt
  must pass the program's existing maintenance checks.

Sizing helpers use bigint token atoms, a supplied value basis and monotone
same-snapshot internal execution quotes. They are estimates, not risk admission
or a promise of capacity. They expose `requiresSimulation: true`; custom quote
callbacks must include swap/collateral-transfer fees and must not charge a
wallet transfer fee on internally borrowed input. Use local/cached quotes to
avoid an RPC for each search iteration. A lower target that cannot be achieved
by a funded increase requires a separate repayment/decrease-size action.

Call `dusk.get.previewLeverageAction({ market, leveragePosition, instructions })`
on the actual transaction before signing. It runs the real instruction, captures
post-execution market/position state and CPI events from one bank, and returns
current stored IM/MM, symmetric-EMA metrics, compute, and gross wallet debits /
net wallet credits per mint. Execution errors are surfaced with simulation logs.
It supports minimum context slots, timeout/cancellation and active lookup tables.
Missing CPI events are an error, not a zero-payout quote. It does not sign or send.
Wallet flows describe the Dusk position action; separately composed zaps and
network fees need their own wallet-flow accounting.

EMA metrics are mark valuations without an executable liquidation fee haircut;
they are not liquidation eligibility, an exact liquidation price or a close payout.
Use the protocol's liquidation previews for eligibility and settlement terms.

## Reaching 1x and borrowing again

Repayment can clear all debt while leaving held collateral. A decrease-size
sale must leave some held tokens; full liquidation of the user's own position
uses remove-from-position instead. Once shares reach zero:

1. Principal must also be zero. Remove all remaining collateral from leveraged
   crowding totals and clear its retained admission obligation.
2. No new debt interest accrues and debt-driven liquidation is ineligible.
3. The owner can withdraw the held tokens with `withdrawRepaidLeverageInstruction`,
   or sell a partial/full slice through owner close and receive the opposite token.
4. Increasing again admits the **entire** position at current depth/crowding and
   fresh stored terms. It cannot restore a stale opening allowance or only count
   newly purchased collateral. A positive wallet-funded, zero-borrow addition
   remains debt-free and does not enter leveraged exposure totals.

Existing position/session locks apply to all these paths. Reaching 1x is not a
new account or a reset of position identity. Initial leveraged entry still
requires borrowing; a debt-free position can be reached by reducing or repaying.

## Amounts and interface changes

- `IncreaseLeverageArgs.margin_amount`: gross owner debt-token funding (zero for
  increase-size). Optional `owner_debt_account` is mandatory for positive funding
  and must match the position owner and debt mint. The program measures net credit.
- `DecreaseLeverageArgs.min_repay_out`: minimum actual internal cash repayment.
  `max_surplus_out`: maximum **gross** unused output refundable to the owner;
  default zero in the SDK. A 1x quote can explicitly allow its rounding surplus.
  The added `owner_debt_account` must belong to the owner and match the debt mint.
- `CloseLeverageArgs.close_bps`: 1..10000, with SDK default 10000. The existing
  `min_amount_out` protects **net owner receipt**, including output transfer fees.
  Percentage is of the position at execution; preview again if it has changed.
- `LeveragePositionUpdated.owner_debit`: gross wallet funding, distinct from
  borrowed amount. `owner_credit` reports net refunds/payouts. Consumers must use
  these flows for contribution-aware PnL; debt repayment is not trading profit.
- A repaid position can emit an unquoted `closeout_value = 0` on reduction.
  Use explicit marked metrics or an execution preview rather than interpreting
  zero as a worthless position.

`leverageRepaymentQuote` reproduces indexed-share/aggregate rounding. Full cash
repayment may differ from the displayed indexed debt by one token atom.
`planLeverageRepayAll` selects the smallest quoted sale covering it and exposes
the surplus bound. Adverse movement can leave the minimum unmet; favorable
movement can exceed the surplus bound. Both fail atomically rather than silently
leaving debt or returning unapproved equity. Refresh the quote before retrying.

Interest/referral routing, swap-fee policy, hLP reconciliation and delegated
close fee policy are preserved. Owner partial closes reuse the existing
proportional settlement engine. Delegated closes retain their existing
positive-debt requirement and callback authorization.
