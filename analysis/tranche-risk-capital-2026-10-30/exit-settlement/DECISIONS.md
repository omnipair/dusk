# Coverage, queues, pricing and tranche accounting

Updated 31 October 2026 after the user's seed and fee-routing corrections. This is
the current decision record. The [plan](../PLAN.md) contains the implementation
sequence; the [calibration review](../calibration/review.html) retains historical
parameter comparisons. [The V1 controller note](../v1-controller/README.md) defines
the current configurable defaults. No native tranche changes have been made.

## Accepted decisions

- All lending paths are covered: ordinary borrowing, leverage and hLP funding.
  All srLP receives senior protection, including leveraged holdings and hLP backing.
  hLP cannot enter junior. Only ordinary, unleveraged LP capital can become jrLP.
- Recover collateral, borrower equity and applicable internal claims before the
  residual waterfall: **available protocol insurance → jrLP → srLP**. Insurance is
  asset-specific and subject to its draw rules. Ordinary inventory losses remain
  LP exposure. Senior protection does not forgive a leveraged holder's debt.
- Split **all net LP-directed interest and swap fees 50/50** between senior and
  junior. Interest includes ordinary borrowing, leverage and hLP funding. This
  supersedes the earlier pro-rata swap allocation.
- Redirect **only the liquidation amount currently filling insurance** entirely
  to junior. Keep liquidator execution rewards and the protocol cut unchanged.
- Use a **funded permanent junior seed** whose received income compounds into
  junior. The initial creator allocation defaults to 0.1%.
  The earlier no-junior-to-senior income fallback is superseded.
- With no live junior, new collected junior revenue builds protocol-owned funded
  junior in a fresh generation. No later depositor receives a historical rewards
  windfall. Old earned income stays with its exposure-time owner.
- Allow new borrowing at zero junior, subject to cash limits; insurance and senior
  bear the remaining risk. Do not impose a junior launch gate.
- Native tranche conversion burns the source claim and mints the other at current
  value. Junior exit requests escrow existing jrLP; they do not create srLP early.
- Queued junior remains counted, paid and slashable until actual conversion. Use
  7–10 days of notice and a two-day exercise window. Missing the window requires
  fresh notice. Cash withdrawal is a separate operation.
- Preserve unused exercise time when a protocol-verifiable unresolved insolvent
  liquidation blocks conversion. Reliable detection and anti-griefing rules are
  still required; the clock alone does not solve them.

## Proposed parameters and queue details

The working two-asset signal is `(funded J / consolidated D) * (1 - U_asset)`, with
one shared junior budget. A senior cash withdrawal leaves J/D unchanged but raises
utilization, reducing this combined signal. A lower signal below target raises
rates; a higher signal above target lowers rates. Inside the band rates hold.

The user selected **20–25% of total LP capital in junior**, expressed at 80%
reference utilization as fixed combined-signal bounds of **5–6.25%**. Recomputing
the band using live utilization would cancel the desired liquidity response.

Use the V1 exponential band controller. Both bounds and the half-life are market
parameters with governance updates. Defaults are 4% initial APR, 2% minimum and
no configured maximum, with a fourteen-day half-life. Initial APR, minimum and
maximum are also optional market-creation inputs: omitted values use the defaults,
and supplied valid values override them. Governance can replace these parameters
prospectively. The separate new-borrow utilization default remains
80% per asset. The earlier 1%-minimum, 50%-maximum recommendation is
superseded; its simulation outputs remain historical evidence. No further search
for one optimal immutable configuration is required.

Governance model replacement must settle the old interval before switching and
preserve ongoing rates. New-market initial APR is not an update reset. Correct
the V1 coefficient precision issue found in the source review. These requirements
are tested in the new mathematical reference, not yet integrated into native Dusk.

The queue prototype uses:

```
notice = 7 days + 3 days * min(queued junior units / total junior units, 1)
window = 2 days of available exercise time after notice
```

It includes new requests in crowding, counts expired escrow until it is cancelled
or renewed, prevents additions from shortening notice, and permits cancellation
only once notice has ended. Cancellation returns junior. Crowding does not impose
a daily conversion cap or FIFO release schedule. All pending units remain exposed
to junior losses, including during an insolvency restriction.

The selected 5-bp fee transfers jrLP units to the permanent seed. New requests pay
on new units, additions before maturity pay on the addition, and renewal after
maturity pays on the entire renewed amount. The user accepted the remaining settings after revising the controller defaults.
Giving the fee back to remaining junior holders can refund most of it to a
dominant requester. Transferring it to the permanent claim avoids that direct
ownership rebate, but does not prove strategic exits are uneconomic.

## Seed, income and exhaustion

The seed must come from actual creator liquidity. Unbacked debt shares cannot
absorb losses. Its nonredeemable claim needs an explicit permissionless income
reinvestment path; sending rewards to an inaccessible address would strand them.
The seed's received income mints additional junior at current NAV. The permanent
share is not a guarantee of positive funded capital or a fixed percentage forever.

If losses exhaust junior, old units can still physically exist with no principal
value. Recapitalization must use a new economic generation and prevent those old
units from acquiring new principal or newly earned income. Do not merely zero a
supply counter while old fungible tokens remain valid. Income earned before the
wipe remains separately claimable to the extent it is actually collected.

Newly earned junior-directed fees in the zero-junior state belong to the protocol
seed. Actual collection supplies the tokens to rebuild it. Accrued unpaid interest
is neither spendable cash nor funded underwriting. Subsequent users enter at the
current funded value and receive only prospective income rights.

The exact prototype freezes income rights at exposure time. Partial collection is
allocated across earned rights, transfers do not take the seller's earlier income,
and uncollectible interest is written off without another funded-principal charge.
Per-loan income preparation and claims are paged, capped at eight records per call.
This is bounded work per call, not bounded lifetime storage or a complete Solana
account design. Empty-senior routing to a reserve remains an unapproved placeholder.

## Protocol references and differences

Inverse's published relaunch used 1–10 days of notice, a two-day window and a 5-bp
queue fee. Its verified escrow prevents cancellation before the window begins;
afterward cancellation returns junior and completion redeems underlying. Expired
escrow continues to affect crowding. It has neither FIFO execution nor a global
daily release cap. Omnipair's proposed 7–10-day range, conversion to senior,
preserved exercise time during restrictions and fee recipient differ from that
implementation. Do not describe them as a universal standard.

Sources: [relaunch proposal](https://forum.inverse.finance/t/proposal-to-relaunch-jrdola-with-initial-dbr-reward-budget/666),
[verified escrow](https://github.com/InverseFinance/JuniorDola/blob/727bb0877ae8067cbe49bdea8964dba67d2f1b40/src/WithdrawalEscrow.sol),
[local source comparison](../inverse-exits/RESEARCH.md).

Monolith uses current redeemable free debt, including normalized free PSM assets,
divided by paid plus adjusted free debt. Mode changes accrue interest before
changing the ratio. There is no pending junior signal. Liquity V2 has no equivalent
Stability Pool notice queue and uses borrower-set borrowing rates. Neither proves
our queue or controller immune to manipulation.

Liquity V2 deliberately accumulates minted yield while Stability Pool deposits are
below its minimum. Omnipair lends tokens it cannot mint, and its seed is funded by
actual collections. These are different bootstrap mechanisms.

Sources inspected at repository revisions, not newly matched deployed bytecode:
[Monolith Lender](https://github.com/MonolithMarket/Monolith/blob/3511ab54c7532efde0fe19423cb3753ea469a805/src/Lender.sol),
[Monolith controller](https://docs.monolith.market/protocol/interest-rate-controller),
[Liquity V2 StabilityPool](https://github.com/liquity/bold/blob/d262669ef3c4d37bf0544df3bb2624c07b8a5ee0/contracts/src/StabilityPool.sol),
[Liquity V2 ActivePool](https://github.com/liquity/bold/blob/d262669ef3c4d37bf0544df3bb2624c07b8a5ee0/contracts/src/ActivePool.sol).

## Evidence and remaining work

The current reference is [calibration/accounting.py](../calibration/accounting.py),
with 31 combined accounting/controller tests and 1,514 synthetic simulation runs,
including repeated baseline configurations. Earlier `ledger.py` and settlement comparisons are historical; their
empty-junior income fallback is superseded.

The tests check exact two-token balance conservation, seed funding, shared junior
coverage, historical income, partial collections, writeoffs, new generations and
window timing. They do not implement native integer rounding, complete hLP state
transitions or a verified insolvency detector. The model supplies accrual snapshots
explicitly; native integration must derive them from verifiable debt and ownership
epochs and establish affordable account access for long-idle loans.

Before native integration, resolve insolvency activation/clearance and repeated
dust griefing, income-history storage/catch-up costs, atomic token-generation
rollover and source-specific hLP accounting. Then run native transition tests,
regenerate interfaces and complete repository CI. Calibration results expose
trade-offs; they do not establish a permanently safe coverage floor.
