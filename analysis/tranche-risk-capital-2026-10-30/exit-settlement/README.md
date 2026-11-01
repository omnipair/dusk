# Junior exits: scheduled settlement or a limited exercise window

31 October 2026. Design analysis, not an implemented contract. Only ordinary,
unleveraged LP capital may enter junior; hLP remains excluded.

Recommendation: use a finite notice followed by a limited conversion window for
the first tranche implementation. Scheduled batch conversion is feasible in
principle, but exact deadline settlement adds accounting and execution obligations
across the market. This recommendation does not resolve loss recognition for
either policy, and a limited window bounds rather than eliminates exit optionality.

## How scheduled conversion could work

1. Escrow a specified number of jrLP shares in an irrevocable exit request. Assign
   it to a future settlement batch. The shares retain junior income and first-loss
   exposure until the defined economic settlement boundary. Do not promise a fixed
   srLP amount or immediately subtract them from the pricing coverage numerator.
2. At settlement, reconcile the applicable income and loss state. Convert all due
   shares in the batch at one exchange rate. Burn their junior units and create
   senior units owned by the batch escrow for its beneficiaries. A request cannot
   be selectively left junior because its owner has not clicked a claim button.
3. Count escrowed senior units in senior supply and rewards from settlement onward.
   Their beneficiaries can collect later without changing their economic exposure.
   They receive senior units, not a fixed dollar IOU. Senior can still lose value.
4. Retain per-batch conversion results and reward checkpoints so one settlement
   need not iterate over every holder. Individual claims distribute the batch's
   senior units and earned rewards. Rounding dust, zero NAV, and storage reclamation
   require explicit rules. Settled batches cannot be overwritten while claims exist.

For positive supplies and positive senior NAV, in a common valuation unit:

```
value_converted = queued_jr_shares * junior_NAV / junior_share_supply
sr_shares_issued = value_converted * senior_share_supply / senior_NAV
```

The conversion preserves total funded NAV and cash. Outstanding conditional rights
to interest earned before conversion must be preserved separately until collection
or writeoff; unpaid interest must not be treated as funded principal. This requirement
already follows from the agreed time-of-exposure income rule, under either exit policy.

The distinction between a processed request and later token collection also exists
in [ERC-7540](https://eips.ethereum.org/EIPS/eip-7540). That standard does not specify
our tranche loss allocation, deadline valuation, or Solana implementation.

## The two possible settlement clocks

**Exact deadline.** Economic exposure changes at the assigned boundary even if the
transaction arrives later. Every relevant market operation must first reconcile
any missed boundaries in order. Debt/rate accrual and reward eligibility must be
partitioned around those boundaries, and senior units owed to unclaimed holders
must already affect accounting. A later claim cannot choose between historical
and current conversion prices. A keeper improves execution but is not the rule.

Dusk is oracle-less; market prices come from internal curve state, so it is wrong
to assume this necessarily requires a new external historical oracle. Where no
market state changed, time-dependent accounting may be reconstructed from saved
state and configured rules. However, that requires all relevant operations to
respect the boundary and does not reveal unrecognized loan losses.

**First protocol checkpoint after the deadline.** Conversion occurs at the actual
checkpoint value, and junior income/exposure continue until then. This avoids
backdating, but it is a different promise: eligibility is time-based; settlement
time is activity-dependent. To prevent a holder keeping junior exposure indefinitely
in an active market, due batches must be processed before relevant operations,
without holder veto. An idle market can still leave an overdue position junior.
That idle interval preserves some timing optionality; it is not exact automatic exit.

Both versions need a bounded batch-processing design. Processing every overdue
request inside a swap is unsuitable. Aggregation avoids holder enumeration, but
multiple missed epochs, required accounts, and compute limits still need handling.
Requiring catch-up before normal operations creates a market-liveness dependency;
letting operations bypass it can break the promised exposure cutoff. Liquidation
and repayment progress must be addressed explicitly. No such mechanism was added.

## Loss ordering is shared work, not a problem solved by a claim window

Example: senior principal 80,000, junior principal 20,000, and a request covering
half the junior shares. Ignore fees, inventory changes, and income for this example.

| Processing order | Value moved to senior | Junior remaining afterward |
| --- | ---: | ---: |
| Recognize a 4,000 loss, then convert | 8,000 | 8,000 |
| Convert, then recognize the same 4,000 loss | 10,000 | 6,000 |

The order shifts 2,000 between the exiting and remaining junior holders. Under a
recognition-time policy these are different event sequences with different owners;
they are not automatically an arithmetic bug. The problem is promising protection
through an earlier economic cutoff while silently assigning losses by whichever
transaction happens first. A loss only discovered later cannot be backdated fairly
without a policy and evidence about when it belongs.

Current Dusk realizes an ordinary public liquidation's residual loss when collateral
is exhausted, using the specific borrow position. `Market::update` accrues debt and
updates market/hLP/risk state; it does not settle every outstanding borrower. Thus
"settle all known losses first" is not an existing constant-time primitive. A window
also needs honest rules for recognized losses, active liquidations, and late loss
discovery. Extending an exit indefinitely until every potential loss is resolved
would change the commitment users were promised.

Relevant source inspected:
- `programs/dusk/src/transitions/lending/mod.rs`, `accrue_side`, `Market::update`.
- `programs/dusk/src/transitions/lending/liquidation.rs`, `Liquidation::apply`
  (residual loss calculation after collateral exhaustion).
- `programs/dusk/src/transitions/liquidity/hlp/mod.rs`, `HlpYieldEligibility`: current
  hLP funding rewards explicitly use payment-time ownership, which differs from the
  proposed exposure-time reward rule. hLP's exclusion from junior does not remove
  that accounting reconciliation.

## Smaller initial design: notice and a limited conversion window

Escrow jrLP on request. It continues junior income and first-loss exposure. After
notice, the owner or an owner-authorized executor may convert during a finite window
at the current settled value. Conversion burns jrLP and issues srLP atomically;
cash redemption remains separate. Do not give arbitrary callers discretion to
exercise somebody else's optional window without authorization.

After expiry, eligibility ends. The shares remain junior; nothing turns them into
senior automatically. The holder can recover the junior receipt or renew the request
with a fresh full notice. Renewal must replace the old eligibility, not create
overlapping windows on the same shares. Transferring or splitting the position must
not reset the age of escrowed shares or duplicate a request. Durations are unselected.

This resembles the verified Inverse escrow's finite completion window and retained
junior shares on expiry; it does not imply adopting Inverse's fees or governance pause.
See [verified source findings](../inverse-exits/RESEARCH.md).

For illustration only, seven days of notice plus a two-day window gives a repeatedly
renewed position an exit option for 2/9 of each cycle. Staggering many equal positions
can keep roughly 2/9 of capital eligible continuously, ignoring fees and crowding.
Users may still prequeue. The improvement is a bounded option, not elimination of
strategic timing. An initial fee could price that option but is not selected here.

Crowding-based delays do not impose a conversion-volume limit. A strict requirement
to spread large departures needs a global release budget or scheduled partial batches,
which is another design choice rather than a property of the limited window itself.

## Decision

Scheduled conversion has coherent token accounting once the economic boundary is
defined. Exact timing is the expensive part, especially across debt accrual, loss
recognition, and missed batches. Given the user's preference to use a window if the
alternative is messy, recommend the limited-window design for the initial model.
Leave notice/window durations and loss-event restrictions explicitly open. Do not
claim that either policy has been proven safe in the full two-asset Dusk engine.

`python3 check_examples.py` reproduces the loss-ordering example, delayed delivery
of senior claims, batch allocation, reward differences under late settlement,
repeated-window eligibility, and coverage changes. It uses exact fractions in a
single-numeraire principal ledger. These checks do not exercise the Rust program,
interest receivables, AMM inventory, integer rounding, or Solana execution limits.
