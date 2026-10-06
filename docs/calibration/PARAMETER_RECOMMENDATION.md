# Margin and liquidation parameter recommendation

2026-10-07 · PR #45 · **Proposal awaiting the remaining product decisions.**

The [current decision record](../LIQUIDATION_DECISIONS.md) remains authoritative.
This recommendation adds concrete starting numbers; it does not claim the
redesign is implemented or ready for deployment. Evidence: the corrected native
engine's [1,080 parameter comparisons and 48 residual checks](PARAMETER_RESULTS.md),
plus the earlier [576 concentrated/hLP scenarios](CONCENTRATED_CUSHION_RESULTS.md).

## 1. Margin curve

Recommend **7% / 12% / 17% marginal maintenance**, with band boundaries at **5%
and 15% of stored side depth**. Apply each rate only to collateral in that band.
Keep the selected **3 percentage point IM buffer**. There is no discontinuous
increase in required equity when a position crosses a boundary.

| Position collateral / stored side depth | Effective MM | IM before crowding | Reference leverage ceiling |
| ---: | ---: | ---: | ---: |
| 5% | 7.00% | 10.00% | 10.00× |
| 10% | 9.50% | 12.50% | 8.00× |
| 15% | 10.33% | 13.33% | 7.50× |
| 30% | 13.67% | 16.67% | 6.00× |
| 50% | 15.00% | 18.00% | 5.56× |
| 100% | 16.00% | 19.00% | 5.26× |

These are rounded mathematical ratios. Integer requirements round conservatively.
For a $50,000 stored side reference, 30% means $15,000 of held collateral at the
reference price. It does not mean a $15,000 wallet deposit or a $50,000 pool.
Actual entry execution, fees, cash, reference health and crowding can require
more equity. All single $15,000-spend requests at approximately 6× or 5× failed
admission in the new fixture sweep. Smaller requests sometimes passed. The
selected 6× **reference-margin** target therefore must not become a UI promise
of 6× executable wallet leverage at that size.

### Stored reference and crowding

Use the existing conservative candidate: the smaller of collateral-side physical
cash and debt-side physical cash converted at the reference price, scaled down
when pessimistic depth EMA is below current curve depth. Observe the post-entry
state. Do not count receivables as cash or multiply inventory by amplification.
Store the reference in collateral atoms, so changing token price does not rebase
the maintenance bands. Later liquidity changes affect admission of new risk,
not an existing position's stored bands. Increased risk rechecks terms and
previews the result; shared exposure must include all outstanding positions and
active flash sessions.

Recommend the gentle crowding candidate:

```text
crowding IM = min(100%, 10% + max(0, same-side exposure / current conservative depth - 20%) / 10)
required IM = max(effective position MM + 3 percentage points, crowding IM)
```

| Same-side exposure / conservative depth | Crowding IM |
| ---: | ---: |
| 20% | 10% |
| 40% | 12% |
| 60% | 14% |
| 100% | 18% |

The requirement falls again when exposure falls or conservative depth recovers.
It does not change existing positions' maintenance. Doubling the slope did not
change admission in this sweep, which is insufficient to identify an optimal
slope. Additional aggregate exposure and splitting tests are required during
implementation. Removing the 2% unwind rule still requires replacing its runtime
guard and verifying every risk-increasing path; this proposal alone does not
remove it.

## 2. Liquidation package

| Setting | Recommended starting value | Consequence |
| --- | --- | --- |
| Ordinary buyer discount | Selected 0.5% → 5%, linear from MM to zero EMA equity | Buyer pays a fixed net amount and retains execution upside on the purchased slice. |
| Partial recovery target | Remaining position MM +2 percentage points | Limits repeated near-threshold cleanup while remaining below the +3-point entry buffer. Accept smaller useful fills; cap excessive seizure. |
| Insurance contribution | 0.2% of partial purchase payment; on full solvent closure, at most 0.2% of debt and no more than surplus after debt repayment | Included in the borrower's cost and recovery calculation; never creates a shortfall merely to fund insurance. No contribution on an insolvent full close. |
| Solvent internal full close | EMA eligible and final net repayment cushion between 0% and 2% of debt | Entire principal and interest must be paid from execution. No insurance draw or interest cancellation. Residual belongs to owner. |
| Separate loss-taking permission | EMA equity at or below 75% of applicable MM | At 7% MM, permission begins at 5.25% equity. Allows a full forced sale sooner than the 50%-of-MM alternative (3.5% equity). No mandatory wait. |
| Internal caller reward | Linear 0% → 1% of actual proceeds as health moves from MM to zero equity; capped at 1% when insolvent | At the proposed 75%-of-MM boundary the rate is 0.25%. At half MM it is 0.5%. External flash buyers instead receive the collateral discount. |

Example: a $1,000 partial purchase contributes $2 to insurance and credits $998
toward debt, before any separate interest distribution. All quoted transfers
must be grossed up as needed to achieve the specified spendable credits.
Example: an internal sale receiving $4,500 at zero equity pays at most $45 to
its caller; $4,455 remains for principal-first recovery. Insurance covers only
unrecovered principal within its limits; unpaid interest is canceled without
fees, and remaining uncovered principal is written off atomically.

The earlier boundary reduced write-off in 123 of 144 comparable funded stress
runs and left it unchanged in 21; none worsened on these paths. This is not a
general guarantee: all paths decline, and an earlier forced close can hurt a
borrower whose market would recover. Its measured benefit includes paying a
smaller reward earlier in the distress curve. Keepers can choose an authorized
full close; an off-chain preference for partials does not enforce priority.

Keeping the 50%-of-MM boundary is a valid product alternative. It preserves more
time/opportunity for partial repair and accepts later execution on a falling
market. Negative AMM cushion alone never authorizes loss-taking.

The 1% reward cap favors the previously selected cleanup incentive. The 0.25%
cap alternative usually retained more recovery when execution occurred, but
the model assumes keeper action above a fixed cost and cannot establish equal
real-world participation. These rewards are not a minimum guaranteed bounty.

## 3. Dust is the remaining funding choice

All 32 eligible positions left in 22 earlier runs had an allowed fill when
modeled keeper cost was zero, but no selected fill at a cost of 0.05 debt tokens.
Total residual debt per affected run was 0.146–2.605 tokens. At the fixture's
initial $1 debt-token value those are cents-to-dollars balances, not large
positions stranded by an accounting guard.

A capped percentage reward cannot ensure cleanup of arbitrarily small amounts.
The protocol also lacks a universal dollar conversion for arbitrary debt mints.
Do not silently treat 0.05 debt tokens as a $0.05 on-chain minimum or promise
that keeper competition resolves negative-profit fills.

**Recommended initial scope:** prevent avoidable dust from partial fills with
an explicit per-market residual-size rule, allow a full fill under the applicable
payment/loss permissions, and retain percentage-based rewards without an
automatic insurance subsidy. Document that naturally occurring tiny balances
can remain economically unattractive. Full closure must not bypass the price
obligation or loss permission merely because the position is small. Exact
residual-size units/threshold still require selection; a stored-depth percentage
alone is not a network-cost estimate.

**Alternative:** fund a small bounded cleanup reward from insurance. This requires
an explicit denomination, budget, lifetime cap and splitting/reopening abuse
analysis before implementation. Merely increasing the percentage cannot give
the same guarantee. A bounded budget can also be exhausted; even this choice
does not guarantee unconditional cleanup.

## 4. Implementation and verification boundary

The parameter experiment is passing; **the full redesign remains unimplemented**.
It uses native swaps, fees, EMA, recentering, repayment, insurance, write-off and
hLP reconstruction with interest frozen. It does not validate principal-first
allocation with accrued interest, token fees, borrowing positions, atomic
session instructions, arbitrary routes, account aliasing, same-transaction
shared mutations or deployment compute cost.

1. Record approval or revisions to the margin and liquidation package; settle
   dust funding and denomination. Do not silently make these runtime defaults.
2. Implement bound sessions, token-net-payment quotes, position locks and
   shared reservations for borrowing/leverage; preserve permitted retail and
   LP/parameter composition with current-state settlement checks.
3. Wire progressive margins, partial recovery, solvent/emergency permissions,
   principal-first loss handling, insurance and owner/delegate payouts. Replace
   auctions; regenerate SDK interfaces and previews.
4. Run adversarial instruction tests and the full required CI, audit the
   integrated design, then perform the separately authorized devnet rollout.

Finding **#287652 remains open**: the native economics and atomic settlement do
not prove that internal emergency execution cannot be manipulated. Track the
accepted execution/loss tradeoff and adversarial evidence explicitly; passing
accounting tests cannot close that finding.
