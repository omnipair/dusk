# Senior exits, trapped junior capital, and interest feedback

The user correctly challenged the earlier explanation: zero cash also occurs
under utilization pricing. It does not establish that tranching is worse or that
utilization guarantees instant exits. The distinct issue is the controller's
response to the shortage and whether junior balances represent voluntary,
continuing underwriting supply.

## Exact balance-sheet example

| Quantity | Before | After senior withdraws 20 cash |
|---|---:|---:|
| Cash | 20 | 0 |
| Loans | 80 | 80 |
| Senior claims | 80 | 60 |
| Junior claims | 20 | 20 |
| Junior / all LP equity | 20% | 25% |
| Junior / senior | 25% | 33.33% |
| Junior / debt | 25% | 25% |
| Utilization | 80% | 100% |

The cash shortage is shared with the current system. The raw participation
controller nevertheless sees apparently excessive junior and lowers rates;
the utilization controller sees scarce cash and raises rates. Junior/debt
correctly sees unchanged nominal loss-bearing capital against the same loans.
In an actual AMM, thinner execution liquidity can additionally worsen recovery;
that effect is absent from this fixed-recovery example and belongs in stress
weights. A lower interest rate itself does not prove that defaults become more
likely; it may reduce debt-service burden while weakening LP incentives.

In a one-asset funded book, let q = J/(S+J) and U = D/(D+C). Because S+J = D+C,
the identity `J/D = q/U` holds. A raw junior-share target implicitly tolerates
different coverage per loan at different utilization levels. Loan repayment
changes D and C but does not directly change S or J, another reason the raw LP
ratio is a poor standalone exposure signal.

Monolith's free-debt ratio has a different denominator and contract: paid-debt
repayment increases the remaining free-debt share, and free borrowers provide
actual collateral-redemption capacity. Its redemption consumes Coin, retires
eligible debt and delivers collateral. That is not first-loss insurance.
Sources: [controller](https://docs.monolith.market/protocol/interest-rate-controller),
[redemptions](https://docs.monolith.market/protocol/redemptions), checked 2026-10-30.

## The proposed loop can occur, but define both exits

Senior withdrawal -> junior share rises -> raw-share rate falls -> junior may
want less exposure -> cash redemption cannot execute -> measured junior balance
stays high -> controller continues seeing apparently abundant underwriting.
This is a plausible feedback failure, not an inevitable prediction of investor
behavior. A trapped balance is not evidence of fresh willingness to underwrite.

Notice-requested capital should cease supporting additional commitments, even
while it remains loss liable on the existing book until the agreed exit. The
signal must distinguish present loss-bearing capital from future committed
capital. Cancellation rules and real commitment durations matter; a costless
cancelable notice could otherwise manipulate compensation.

Exiting junior underwriting and withdrawing tokens are separate actions. If the
contract permits an in-place junior-to-senior conversion after notice, it can
end first-loss exposure without removing cash. Coverage falls, but assets and
total LP claims do not. If a coverage floor blocks that conversion, junior can
remain committed indefinitely. Ordinary LP cash redemption still waits for
actual assets in either design. Known losses must be recognized before any
conversion to avoid escaping an already incurred loss.

## A matched controller comparison, not proof of a better protocol

Two exercises isolate the pricing question:

1. `compare.py`: exact book states held for seven days; same initial 8% APR.
   Compare utilization, raw junior share, commitment-aware share, committed
   junior/debt, and committed junior/debt with a utilization-derived rate floor.
   The utilization curve uses Dusk defaults (70% target, steepness 4, annual
   adaptation 20) in a continuous-time approximation. No market response is
   assumed. The unchanged starting book is already at 80% utilization, so the
   utilization anchor grows even without a new shock.
2. `dynamics.py`: 180 synthetic runs, five controllers × six scenarios × six
   behavior settings, using the existing hybrid engine's loan and cash ledger.
   All arms have the SAME $10m funded capital, $2m productive junior, zero
   segregated reserve, $8m initial debt, first-loss waterfall, seven-day junior
   cash notice, and collected-interest allocation. This is **not** current
   Dusk insurance versus a new tranche system; it isolates rate policy within
   one proposed tranche contract.

In the dynamic runs, principal outstanding is demand-capped at $8m; initial
borrowing is already at that cap. The cash/debt ratio is single-asset, with no
traded AMM inventory. Interest can capitalize, but never funds insurance until
collected. Borrowers voluntarily repay from outside wealth under the assumed
response function. There are no new LP deposits or endogenous underwriting
choices. Junior cash exits are exogenous requests, and the conversion scenario
models a previously noticed conversion maturing on day 20. No hard coverage
admission check is enabled, to avoid attributing its effect to pricing.

The combined policy here is `rate = max(coverage_rate, liquidity_rate)`.
It is an experiment with two constraints on ONE borrower rate, not a settled
recommendation to sum two full IRMs. Fixed revenue allocation across arms avoids
confounding pricing with changes to junior compensation. Other combinations,
including a coverage-driven rate anchor with an immediate utilization curve,
remain candidates; none has been established optimal.

## Findings and adverse outcomes retained

Holding the senior-exit book fixed, the raw share rate falls from 8% to 6.73%
after seven days. Junior/debt stays at 8%. The utilization-shaped rate rises
immediately to 16% and reaches 23.48% after seven days; the combined floor follows
it. These numbers depend on the declared response speeds and are not forecasts.

Base dynamic scenario, senior exits on day 20 and junior requests $1m on day 21,
with no crash:

| Pricing | Junior cash paid by day 90 | Still queued | Average debt | Debt-weighted quoted APR |
|---|---:|---:|---:|---:|
| Utilization | $1.000m | $0 | $5.940m | 11.31% |
| Raw junior share | $0.092m | $0.908m | $8.007m | 4.86% |
| Commitment-aware junior share | $1.000m | $0 | $4.965m | 15.11% |
| Committed junior/debt | $1.000m | $0 | $5.435m | 12.61% |
| Coverage with liquidity floor | $1.000m | $0 | $5.300m | 12.97% |

Counting notices helps even without changing the denominator. A debt denominator
also lets repayments restore adequacy; a raw share target can keep rates high
after junior converts even as the loan book shrinks. Neither fixes cash instantly.
No-voluntary-repayment sensitivity retains the shared inability of pricing alone
to generate principal repayment cash.

Add a day-40 crash to 60% of original collateral price with 10% recovery discount:
utilization produces $273,547 senior loss; coverage alone $161,661; combined
$203,509. Thus the combined policy does NOT dominate coverage alone in this
case. Immediately before the crash the combined policy has $5.066m debt versus
$4.902m under coverage alone, despite lower average debt over the full scenario.
The paths and timing matter.

Against utilization in that base crash, the combined policy has lower senior
loss but higher quoted APR (20.18% vs 11.93%) and lower average debt ($3.642m vs
$5.367m). In inelastic and slow-response cases, higher rates often provide no
additional reduction in the crash loss. Raw-share pricing produces zero senior
loss in the base moderate crash partly because it retains junior capital that
could not exit; this is NOT a costless superiority result for that policy.

The measured benefit is additional information about underwriting scarcity and
better feedback in some states. Improved overall capital efficiency, sustainable
junior participation, and better real outcomes have not been established. Those
require comparable borrower cost, junior return/exit experience, actual loss
recovery, and external capital response. Synthetic behavior assumptions cannot
prove investors will fund the proposed contract.

## Check and reproduce

```
python3 compare.py
python3 dynamics.py
python3 verify.py
```

The independent verifier checks 75,810 identities across 30 base traces, maximum
residual below 0.000001 dollars. No runtime Dusk code or interfaces were changed.
Inspect [state comparisons](controller_responses.csv), [all dynamic results](dynamic_results/summary.csv),
[traces](dynamic_results/traces/), and [verification](dynamic_results/verification.json).
