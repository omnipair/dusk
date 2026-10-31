# Hybrid iteration: capital, cash, and exit promises

This extension is a synthetic mechanism experiment, not a calibrated risk model,
native Dusk replay, or selection of live product policy. The original experiment
remains unchanged. It follows the user's request to keep improving the model and
their question about Monolith and Liquity exit rules.

## Questions fixed before execution

1. Does moving some junior backing into debt-token cash improve loss coverage
   enough to justify less active backing? The reserve is PART OF junior capital,
   never an additional layer counted twice.
2. What do a fixed seven-day exit date and a coverage-preserving exit queue trade
   off, including when borrowers do not repay and nobody replaces junior?
3. Can a bounded risk premium restore protection without an admission constraint?
   Does interest capitalization weaken that response?
4. Can scalar NAV protection be confused with preserving each token claim?

## Balance sheet and coverage promise

Initial total equity is $10m: $8m senior, $2m junior. Initial quote debt is $3m,
lower than the first experiment because this extension stresses ALL loans with
the same collateral shock. Pool inventory includes $2m of base tokens initially
priced at $1; the rest is quote cash or funded quote principal. Reserve cash is
owned by junior and segregated from active backing. Reserve sizes 0/5/10/20% of
total initial equity are tested. These are matched total budgets, not proposed
Dusk parameters. No external underwriting inflows or endogenous LP switching
are assumed: this deliberately tests reliance on voluntary repayment alone.

Two price-gap stress states (60% and 45% of current price) apply recovery
discounts of 10% and 20%. Each loan liquidates at the SAME fixed 85% debt/collateral
LTV, with 70% origination LTV. Quotes incorporate the loss of junior's share of
marked base inventory before absorbing credit loss. Capital adequacy is the
minimum of stressed junior capital / stressed funded-principal loss across the
two states. A 1.10 target is an assumed scenario margin, not a probabilistic
coverage guarantee. Pending exits are excluded from forward capacity immediately.
Queued shares remain loss liable until they leave at current NAV.

Pool assets = quote cash + marked base inventory + funded principal = senior +
junior - reserve. Uncollected interest is tracked as borrower debt but never
counted as insurance capital or pool equity. On repayment/liquidation, cash pays
accrued interest first, then principal; only collected interest is distributed.
This approximates Dusk's realized-interest ordering. Loan recovery discounts are
exogenous; execution depth is NOT used in the liquidation trigger.

For a credit shortfall, reserve cash is transferred to the pool first, then
remaining junior claims absorb loss, then senior. Junior loses exactly the
covered amount; reserve spending is not a second loss. No reserve is magically
refilled. Ordinary inventory P&L is allocated between senior and the junior LP
component, excluding segregated reserve. A wiped junior epoch cannot own new
capital. This is quote-NAV protection, not a guarantee of original base/quote
token entitlements or LP principal against market movements.

Base inventory is held constant except mark-to-market; this is a two-asset
balance sheet, NOT an AMM trading path. Quote cash constrains quote lending and
cash redemptions. A separate static example records why base liquidity cannot
be treated as quote cash without an executable conversion. hLP and internal AMM
liquidation execution are not simulated; their current code is reviewed only.

## Pricing, growth and exits

Borrow APR = 4% funding + a risk premium starting at 4%. The premium changes
exponentially with capital shortfall, with a seven-day full-error doubling time,
a 0.1–25% premium range and a 5% dead band. Only cash-collected premium goes to
junior; cash-collected funding interest is split pro rata over active LP equity.
There are no swap profits in this experiment. A fixed funding rate is a control
assumption, not a replacement for a real funding/liquidity price.

Demand is an assumed rate-elastic target capped at $8m. Voluntary repayments
and new loan demand adjust with a seven-day time constant; repayments are funded
externally. Sensitivities include elasticity zero, slow response, no voluntary
repayment, and fully cash-paid vs fully capitalized scheduled interest. These
are behavior assumptions, not estimates or fair-premium discoveries.

Cross fixed-date / coverage-preserving exits with price-only / admission-checked
new borrowing. Admission must leave stressed committed coverage above target;
it never liquidates an existing borrower or changes the liquidation LTV. The
coverage queue permits partial FIFO exits only if remaining actual capital meets
the same target. A fixed-date exit ignores coverage, but still requires actual
cash. No policy promises an exit when the required token is unavailable.

Scenarios: calm; demand doubling on day 20; 50% junior exit requested on day 30;
that exit followed by a day-40 price drop to 45%; growth followed by that crash;
and the exit followed by a more severe out-of-envelope 30% price crash. Event
order: mark and liquidate already unsafe loans, request/settle exits, update
premium, accrue/pay interest, voluntary debt adjustment/new borrowing, liquidate
rate-induced unsafe loans, settle exits again. Horizon 90 days, one-day steps;
selected base comparisons repeat at six-hour steps.

Report all scenarios and sensitivities; do not choose parameters by outcomes.
Required outputs: senior funded-principal loss, actual junior exits and residual
queues, time beyond notice, debt/initial equity, actual paid vs capitalized
interest, cash, active backing and APR. Coverage ratios are scenario diagnostics,
not loss probabilities. No claims of an optimal model or historical performance.

## Verification

Unit tests cover loss priority, the reserve double-counting trap, pending-share
NAV, no cash exit, protection/exit impossibility with no replacement or repayment,
capitalized interest, rate-induced liquidation, admission and token composition.
Export traces for an independent CSV balance-sheet/flow reconciliation, retain
source hashes, and inspect a standalone figure. Runtime code, SDKs, branches and
commits are outside this research change.

### Supplemental directional check added after the dynamic grid

The dynamic grid models quote-denominated loans, so it cannot answer whether a
reserve in the opposite token protects base-denominated borrowing. A separate
static book sweeps both debt directions and three reserve currency mixes,
holding initial total exposure at $5m base and $5m quote. It includes opposing
55% relative-price shocks and a declared common recovery/default failure with
60% recovery discount at unchanged reference prices. That last case is an
operational/default stress, not a predicted ordinary LTV liquidation. This
extension changes no dynamic parameters. Its output is scenario-feasible debt
capacity bounded by actual debt-token cash, not a proposed capital optimum.
