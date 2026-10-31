# Tranche capital and autonomous pricing — experimental protocol

This is a synthetic economic mechanism experiment, not a Dusk engine replay,
historical backtest, calibrated risk estimate, or proposed production policy.
The user asks whether productive junior LP capital can improve capital efficiency
and LP bad-debt protection without restoring V1's depth-sensitive liquidation trigger.

## Comparisons fixed before execution

All policies start with $10m committed capital, $5.6m debt, and an 8% borrow APR.
The segregated reserve arms have $8m ordinary LP equity and $2m debt-asset
insurance cash; the tranche arms have $8m senior and $2m junior LP equity.
This matches initial debt and borrower price as well as total capital. It does
not match active pool equity: deploying reserve capital is the treatment.

1. Segregated reserve with Dusk-style utilization pricing and 20% event / 50%
   daily insurance draw limits. Each shock contains 20 individual liquidations.
2. Identical reserve with no draw limits, isolating the effect of the limits.
3. Junior capital with a fixed 8% APR, isolating tranching from repricing.
4. Junior capital with a controller targeting 20% committed junior / active equity.
5. Junior capital with a controller targeting committed junior / debt = 2 / 5.6.

Tranche controllers use a 5%-relative dead band, seven-day full-error doubling
time, and a 1–100% APR range. Half of collected interest goes to junior; half
goes to senior. Both receive their pro-rata share of 2% assumed annual swap
income. These are scenario inputs, not estimates. The reserve arms route all
collected interest and swap income to ordinary LPs; endowed insurance earns no
income. We measure outcomes over ALL committed capital, including that reserve.

## Accounting and borrower treatment

The simulator is a one-numeraire balance sheet: pool equity = liquid inventory
value + performing principal. The inventory is assumed convertible into debt
tokens at its marked value. Separate token inventories, internal AMM execution,
hLP leverage, fees on transfers, stale oracles, governance and SVM arithmetic
are outside this experiment. Cash feasibility is optimistic relative to a
two-token AMM and is reported separately from equity coverage.

All interest is collected in cash each time step and distributed immediately;
unpaid future interest never becomes insurance capital. A fixed 70% origination
LTV and 85% liquidation LTV apply to the modeled stressed cohort. A price gap
crosses the same trigger for every policy. Recovery = min(principal, principal
× post-gap price / 0.70 × (1 - execution discount)). The execution discount
affects recovery only, never the liquidation trigger. A marked inventory loss
hits junior and senior pro rata BEFORE the credit waterfall. Performing debt
remains at par until liquidation. There is no capitalized interest or feedback
from rate increases into collateral health; this is a material optimistic limit.

Junior write-downs transfer economic loss-bearing to junior; they do not inject
new cash. Reserve payouts actually transfer cash from insurance into the pool.
The invariant is: pool equity + reserve + cash withdrawals = initial capital
minus inventory loss minus principal loss. Income is outside that balance sheet
because it is paid out. New loans and repayments move cash against principal.

## Behavior and commitment

Debt demand is B0 × scenario demand multiplier × (APR / initial APR)^(-elasticity),
capped at twice B0, with a seven-day adjustment time. Repayment is assumed
available from outside borrower wealth. New borrowing cannot exceed liquid cash.
Existing borrowers are not liquidated merely because liquidity changes.

LPs reallocate existing pool equity between senior and junior; there are NO
external capital inflows. Desired junior fraction follows a logistic function
of the quoted junior-minus-senior income spread minus an assumed underwriting
hurdle. The reference hurdle equals the initial spread, so 20% is an explicitly
assumed calm-state equilibrium. This is not independently observed demand.

Junior exits (including switches to senior) queue for seven days, stay exposed
to losses, and receive execution-time NAV. Exit-requested capital is excluded
from the controller's committed numerator immediately but remains liable until
settled. Cash redemption is constrained by liquid inventory; senior conversion
does not need cash. A wiped-out junior epoch cannot claim later recapitalization.
Risk warnings in the run scenario are exogenous public sentiment, not a forecast
computed using the future shock. Frozen LP allocation is a declared sensitivity.

## Scenarios and uncertainty

Six deterministic scenarios: calm, demand growth, moderate crash, joint severe
crash, junior run before a crash, and repeated crashes. No event probabilities
are assigned. Scenario loss percentages are NOT annual expected losses or VaR.
The base horizon is 90 days at six-hour steps. One-at-a-time sensitivity varies
borrower elasticity, allocation speed, revenue split, exit notice, controller
speed, inventory drawdown, and debt turnover; interaction corners are included.
Report all scenarios and settings, not only the most favorable outcome. No
parameter optimization or significance tests are performed.

Primary outcomes are average outstanding debt / initial total capital and
senior bad debt / initial senior equity. Also retain active backing, liquid cash,
gross principal losses, junior/reserve losses, gross income, initial-capital P&L,
borrowing costs, peak APR, exit queues, and junior capital reallocation. Lower
senior losses caused by less debt must be described as exposure reduction.

### Bounded extension after the initial six-scenario run

The first run exposed a missing comparison: the demand-growth scenario had no
subsequent loss, while the unexpected crash started at equal debt. Add one
`growth_then_crash` scenario: the same day-20 demand increase, followed by the
already specified severe day-40 shock. This tests whether growth-related
exposure differences change loss allocation. Retain all original scenarios.
No demand, loss, allocation or controller parameter is fitted to this extension.

## Validation and deliverables

Retain parameter JSON, code, full summary grid, base traces, event ledgers and
source hashes. Independently reconcile balance sheets and revenue splits from
exported traces. Test loss priority, reserve draw limits, shared triggers,
exit-time NAV, wipeout recapitalization, no-cash withdrawals, and zero debt.
Run step-size checks. Produce a short inline interpretation and research figure.
Do not change Dusk runtime code or SDK interfaces, branches, or commits.
