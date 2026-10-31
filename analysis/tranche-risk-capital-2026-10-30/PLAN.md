# Dusk junior underwriting plan

Updated 1 November 2026. The implementation uses **hLP, yLP and jrLP**.
Native market accounting, instructions, governance configuration and SDK builders
are connected on `feat/junior-risk-pricing`. See [implementation evidence](IMPLEMENTATION.md)
for verification and practical limits, and [the V1 controller specification](v1-controller/README.md)
for per-market defaults. Earlier simulation results remain historical comparisons;
they do not measure the native implementation's economic performance.

## What this changes

V2 removed post-liquidation-price-impact collateral valuation to improve borrower
experience and capital efficiency. The proposed model adds a price for committed
loss coverage without making borrower health thresholds depend on LP withdrawals.
It also lets capital already working in the AMM provide junior protection, instead
of requiring all protection capital to sit outside the productive pool.

Ordinary LPs start in senior and can convert any eligible portion into junior.
The token names are hLP, yLP and jrLP. yLP is the default senior claim. Only ordinary, unleveraged capital can enter
junior; hLP is excluded. All yLP claims receive senior protection, including
holdings through leverage or hLP. Protection covers ordinary loans, leverage and
hLP funding, subject to consolidated accounting of internal claims and debt.

Both tranches retain underlying LP inventory exposure. Junior absorbs covered
credit losses before senior, after available insurance. This does not protect
senior against all token price changes, impermanent loss or unlimited bad debt.

## Accepted economic rules

### Income and the permanent seed

After existing explicit protocol deductions, split **all LP-directed ordinary
borrow interest, leverage/hLP funding interest and swap fees 50/50** between senior
and junior. This supersedes earlier notes allocating swap fees pro rata.

Route the liquidation amount currently used to refill insurance **100% to junior**.
The liquidator's execution reward and protocol cut stay unchanged. Other existing
insurance funding and draw rules stay unchanged unless separately approved.
Referral amounts already contained in protocol allocations must not be deducted
from LP revenue again.

Keep a real, permanently committed junior seed. The default seed size is 0.1% of
the creator's initial deposit, taken from that funded capital, not minted without
backing. Its income compounds into junior. Ordinary holders retain their separate
income entitlements. A protocol-owned nonredeemable claim with a permissionless
reinvestment path avoids stranding rewards at an inaccessible address.

If no live junior exists, new junior-directed collected revenue funds a new
protocol-owned junior generation. It does not go to senior or become a historical
gift to the next depositor. Previously earned income remains with the holders who
earned it. The seed can be wiped out; old worthless junior receipts must not acquire
claims on recapitalized principal or later income.

New borrowing remains allowed when junior is zero, subject to cash admission.
Available insurance and then senior bear losses in that state. There is no junior
launch gate.

### Losses and ownership

Recover collateral, applicable borrower equity and internal collateral claims
first. Then apply the residual covered loss to:

```
available protocol insurance -> jrLP -> yLP
```

Insurance is funded, asset-specific coverage under the applicable draw limit, not
the nominal reserve balance in an unavailable token. Junior is one shared value
budget across both debt books. Never count its full value twice.

Convert yLP to jrLP by burning the source claim and minting the destination at
current funded NAV. The reverse conversion follows the same principle. A
1,000-value position split 70/30 leaves 700 senior and 300 junior value, not 1,300
of total claims. Token quantities need not equal their underlying values. These
conversions do not move tokens out of the pool.

Accrued but unpaid income is separate from funded principal and cash. Rights follow
the ownership and exposure period in which the income was earned, including after
transfer or conversion. Later deposits cannot capture earlier earned interest.
Writing off unpaid income must not charge funded junior a second time. Dusk's
proportional principal/interest partial-repayment rule is retained in the macro
model; this proposal does not switch to interest-first repayment.

### Junior exits

Use 7–10 days of notice followed by a two-day conversion window. Queued capital
remains junior, earns junior income and bears junior losses until conversion.
Count it in current protection; requesting an exit alone does not remove coverage
from the pricing input.

During a valid window, convert at current settled value without a coverage-floor
condition. A user or authorized executor submits the transaction. Cash redemption
remains separate and depends on available tokens. Missing the window leaves the
position junior and requires fresh notice. There is no permanent right to exit at
any later time without renewed notice.

The tested scheduling proposal is:

```
notice = 7 days + 3 days * min(queued junior units / total junior units, 1)
exercise window = 2 available days
```

Include the new request when computing crowding. Additions cannot shorten an
existing notice. Expired units left in escrow continue to count as queued until
cancellation or renewal. Cancellation after notice returns junior, not senior.
Crowding lengthens notice; it does not spread large conversions over several days
or impose a global throughput limit.

The selected request/renewal fee is 5 bp transferred as junior
units to the permanent seed. A fresh request pays on its new amount; additions
during notice pay on the addition; renewing after maturity pays on the whole
renewed amount. Returning the fee to remaining junior holders would partially
refund a large requester. The fee is friction, not proof against strategic exits.

Apply recognized losses before conversion pricing. Restrict conversion during a
protocol-verifiable unresolved insolvent liquidation and preserve unused exercise
time. The prototype implements this clock, but reliable detection/clearance and
resistance to repeated tiny distressed positions remain unresolved native design
work. A global Boolean flag by itself is not a solution.

## Pricing and liquidity

Use one consolidated coverage factor with each borrowed asset's own cash factor:

```
R_asset = (funded junior value / consolidated covered debt value)
          * (1 - utilization_asset)
```

Value both numerator and denominator consistently. Include accrued borrower
obligations in debt, but exclude unpaid income from funded junior. Reconcile hLP
internal debt and collateral rather than presenting gross internal claims as new
capital. A shortage in one asset must not be hidden by cash in the other.

For a one-asset example, J=20, debt=80 and cash=20 gives a 5% signal. A senior cash
withdrawal of 10 leaves J/debt at 25%, but raises utilization from 80% to 88.89%.
The combined signal falls to 2.78%. Junior pool share divided by utilization
simplifies to J/debt; raw junior value divided by utilization is not scale-invariant.

Below the target band, rates rise over time; above it, rates fall; within it, rates
hold. Accrue the elapsed interval using the state that actually existed before a
deposit, withdrawal, conversion, repayment or borrowing changes it. A temporary
deposit must not reprice past time. This signal is an incentive/coverage signal,
not an estimate of default probability.

### Per-market parameters and governance

| Setting | Default starting value |
| --- | --- |
| Signal lower / upper bound | 5% / 6.25% |
| Time to double or halve APR while outside the band | 14 days |
| Initial / minimum / maximum APR | 4% / 2% / uncapped (`max_rate = 0`) |
| New-borrow utilization limit | 80% per asset |
| Initial permanent junior seed | 0.1% of creator liquidity |
| Notice / available exercise time | 7–10 days / 2 days |
| Request/renewal fee | 5 bp to the permanent seed |

The user selected 20–25% junior as a share of total LP capital. At 80% reference
utilization, that corresponds to fixed combined-signal bounds of 5–6.25%. In the
two-asset market this simple equivalence also assumes both assets are at that
utilization. Do not recompute the bounds from live utilization: doing so would
cancel the desired cash-scarcity response.

Both band edges and the half-life are per-market parameters, updatable through
an authorized governance model replacement as in V1. Initial rate, floor and
optional ceiling are model parameters too; there is no configured maximum by
default. Initial APR, minimum APR and maximum APR can all be supplied at market
creation, just like the band and half-life. Omitted inputs use the defaults in the
table. A new market starts at its configured initial APR; replacing its model
later preserves its ongoing rate.
Settle elapsed interest under the old model before applying new settings. Rates
start adjusting immediately outside the band. With no debt, hold the current rate.

Reuse V1's exponential band controller with reversed direction for coverage:
low coverage raises rates, high coverage lowers rates. Remove the separate Morpho
utilization curve. Correct V1's coefficient precision and settle-before-replacement
issues documented in the source review. Market creators and governance can
experiment with settings; stop optimizing a universal immutable configuration.

The cap applies to new borrowing, including the cash effects of leverage and hLP
funding. Interest accrual and withdrawals can take utilization above it; this does
not make otherwise executable withdrawals, conversions or repayments fail. Existing
positions do not become liquidatable simply because utilization changed. Interest
can still increase their debt and affect their health.

For a simple loan with fixed total supplied assets:

```
new principal <= min(cash, max(0, 0.80 * (debt + cash) - debt))
```

More complex internal operations require equivalent post-operation checks, not
blind reuse of this single-loan formula.

## Evidence and limits

The [review](calibration/review.html), [machine-readable findings](calibration/results/findings.json)
and [experiment assumptions](calibration/PREANALYSIS.md) cover 1,514 deterministic
simulation runs, including repeated baseline configurations. These use synthetic LP/borrower behavior,
exogenous losses and specified revenue assumptions. They are not market forecasts,
a native Dusk replay or proof that this controller is optimal.

At a 5% target, a seven-day rate response reduced the largest tested senior loss
relative to fourteen days, but changed direction more often. The slower candidate
was an experimental comparison, not a dominant result or a current default. In one stress scenario, changing LP
response from projected to collected revenue changes senior loss from zero to
about $28.6k on the same initial $1m pool. A small seed does not replace substantial
underwriting participation.

The new [exact accounting prototype](calibration/accounting.py) and controller tests
pass 31 tests. They check two-token balances, a shared loss budget, historical income,
seed funding and exhaustion, fresh generations, conversion and window accounting.
Income preparation/claiming is capped at eight records per call; historical storage
and catch-up transactions still grow. Integer rounding and native compute limits
are not covered by exact rational arithmetic.

Earlier `exit-settlement/ledger.py` results use a superseded empty-junior fallback
and are retained only as historical experiments. The calibration prototype is the
current reference.

## Remaining build work

1. Implement the selected per-market governance configuration and V1 controller
   behavior. Keep numerical settings changeable; the earlier fixed recommendation
   is superseded. The 5-bp exit fee follows the user’s acceptance of the remaining settings.
2. Resolve objective insolvent-liquidation detection and clearance without letting
   repeated tiny positions hold every junior exit indefinitely. Test transaction
   ordering and window preservation against the chosen predicate.
3. Trace the historical-income prototype through native debt indexes, bounded
   account access, long-idle loans, transaction cost and hLP eligibility. Its current
   generic internal-collateral example is not a reproduction of the hLP engine.
4. Implement mint/generation rollover after exhaustion, integer rounding, actual
   token custody and all source-specific fee routes. Resolve empty-senior handling;
   the prototype's reserve fallback is a placeholder, not an approved rule.
5. Integrate into Dusk, regenerate public interfaces, update the SDK, and run all
   repository-required native, LiteSVM and CI checks before any commit. Preserve
   current product scope and fixed borrower health rules.

The LP preview should show senior/junior allocation, current coverage, available
cash, current rates, queued conversion dates and the prospective yield for the
user's selected amount. A tiny seed's annualized yield can be enormous; show the
post-deposit rate and distinguish estimated accrual from collected income. With
no revenue, the seed has no yield merely because its shares exist.

## PR contents and cleanup

Commit the research, documentation, source references, tests, results and required
visual assets with the implementation PR. After validation and commit, remove
task-owned builds, caches and temporary files while retaining the evidence and
identity files. Follow [PR-CHECKLIST.md](PR-CHECKLIST.md).
