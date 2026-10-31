# Underwriting sets price; a cash constraint limits quantity

This iteration follows the user's selected architecture: use committed
junior/debt as the price signal, permit junior-to-senior conversion without cash,
and use an illustrative 80% utilization limit ONLY for new borrowing. It does
not add a utilization interest curve, a utilization rate floor, or modeled
default probabilities to that candidate. Earlier combined-rate proposals remain
comparison work, not the selected candidate here.

## Contract

- Senior cash withdrawal has no direct effect on J/D when junior NAV and debt
  are unchanged. Asset valuation, realized losses and execution changes can have
  indirect effects in a full AMM; this synthetic one-asset example excludes them.
- Junior can request conversion to senior. During seven-day notice it remains
  junior and loss liable, but is excluded from forward committed capital. At
  maturity, settled NAV converts to a senior claim without a token transfer.
  This conversion is not blocked by a coverage-floor condition or lack of cash.
  Known losses must be recognized before the switch. Cash redemption remains a
  separate, cash-constrained action.
- Borrow APR rises below the committed J/D target band, holds inside it, and
  declines above it. The probe starts at 8% APR, targets 25% J/D, uses a 5%
  relative dead band, seven-day full-error doubling time, and 1–100% rate bounds.
  These are illustrative mechanism settings, not calibrated recommendations.
- Junior earns an underwriting share of collected interest as well as its
  ordinary LP funding share. The experiment fixes half of collected interest
  for underwriting, with the other half pro rata over active LP ownership.
  Unrealized interest never becomes funded junior capital.
- New borrowing x must satisfy `(D+x)/(D+C) <= 0.80`, using post-borrow debt
  `D+x` and cash `C-x`. Thus `x <= max(0, 0.80*(D+C)-D)` and `x <= C`.
  Repayment, withdrawals, notices and conversions are not blocked by that rule.
  Existing borrowers are not liquidated because utilization crosses the limit.

The denominator convention matters in real Dusk: use correctly consolidated
debt and executable cash in each asset, excluding fee custody and duplicated
hLP positions. The script models quote loans only. Applying the admission rule
to public loans while ignoring other cash-consuming funding paths would not
establish the same market-wide constraint.

## What an 80% borrowing cap promises

It keeps a newly admitted loan from using the last 20% of the debt-plus-cash
book. It does NOT promise that 20% remains after LP withdrawals or unpaid
interest accrual. At D=80, C=20, a senior withdrawal of 10 leaves D=80, C=10,
U=88.89%; new borrowing is blocked, but J/D and the risk price need not change.
Interest accrual can similarly cross 80% without any newly funded borrowing.

Maintaining U<=80% after every withdrawal would be a different contract: at
the cap it would prevent withdrawals even though cash exists. That is not the
user's proposed rule and is not implemented in this experiment.

When coverage is adequate but the borrow cap binds, borrowing is rationed by
availability at the risk-determined price. There is no extra liquidity price
that must rise to clear all borrower demand. This is a coherent policy choice.
Borrow demand still influences price when actual debt grows relative to junior;
there is simply no separate utilization-driven price component.

Even at a fixed borrow APR, LP yield can respond to utilization. With the
experiment's half-underwriting/half-pro-rata allocation, senior funding APR is
`0.5 * borrow APR * D/(S+J)`. At borrow APR 8%, D=80, S=80, J=20, that is 3.2%.
After senior withdraws 20, S=60 and J=20, it becomes 4%, with no APR repricing.
This supplies an incentive for capital to return if that yield is competitive;
the model does not assume or prove the arrival of new LP funds.

## Comparison and limits

144 synthetic runs compare utilization pricing with/without the same admission
cap, and risk-only pricing with/without the cap. All have identical $10m equity,
$2m junior, $8m initial debt, 8% starting APR, conversion rights, income split
and first-loss waterfall. Six scenarios cross six behavior settings. There is
no segregated insurance reserve, new external LP funding, endogenous LP
reallocation, AMM execution or probabilistic risk estimator. This isolates
price/admission contracts, not all of proposed tranching versus current Dusk.

Debt demand is an assumed rate-elastic target bounded at $12m, adjusting over
seven days (30 in the slow sensitivity). Demand doubles on day 20 in growth
cases. Other cases request $2m senior cash withdrawal, notice conversion of
half the junior shares, or combine growth, conversion and a day-40 price shock
to 60% of original collateral price with 10% recovery discount. Those loss
assumptions do not feed the rate. Fixed 70% origination / 85% liquidation LTV
applies in every arm. Unpaid interest increases debt and can affect health.

Base demand-growth scenario: risk pricing with the cap averages $8.072m debt
at 8.00% quoted APR; utilization pricing with the same cap averages $7.811m at
14.04%. The risk-price cap clips new borrowing requests during 70.25 of 90 days.
This demonstrates quantity rationing without a scarcity-induced rate increase.
It does not prove better real capital efficiency: funding is fixed in this
experiment, and investor willingness to supply at the resulting yields is not
estimated. Outstanding debt includes unpaid accrued interest, so it is not
identical to funded principal.

In the base growth/conversion/crash case, risk-only plus cap produces $897,888
senior loss, versus $917,875 for utilization plus cap. But it also has lower
average debt ($4.653m vs $6.918m) and higher quoted APR (24.64% vs 14.28%). This
case is not a free improvement in both efficiency and protection. All outcomes
remain in the CSV; no parameter search selected a winning scenario.

Every scheduled conversion completes at its current NAV without requiring
cash. Senior withdrawals alone do not affect the risk signal directly. Cash
can still be scarce, and ratios can exceed 80% after permitted non-borrowing
operations. Such excursions are not violations of the admission rule: every
positive loan increment in a capped arm asserts post-origination U<=80%.

The mechanism is market-responsive but not parameter-free. The target band,
adjustment speed, interest allocation, commitment terms and borrowing cap define
the market contract. It avoids having to infer a complete default-probability
model to set prices. It does not avoid valuation, loss recognition, manipulation
resistance or the need to measure real participation.

## Reproduce

```
python3 -m unittest test_experiment.py -v
python3 experiment.py
python3 verify.py
```

Seven boundary/contract tests cover post-borrow admission, withdrawals above the
cap, interest-only crossings, conversion with zero cash, conversion after loss,
and risk-only repricing. Independent exported-trace identities check equity,
principal, cash withdrawals, interest, and conversion entitlements. Read
[all outcomes](results/summary.csv), [verification](results/verification.json),
[source manifest](results/manifest.json), and [base traces](results/traces/).
Runtime Dusk code and interfaces are unchanged by this research.
