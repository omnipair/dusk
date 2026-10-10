# Incremental crowding: focused validation

2026-10-07. **Historical arithmetic study preceding runtime implementation.**
Its exact examples below use the size-band potential alone. The subsequent
[runtime implementation](../STORED_LEVERAGE_MARGINS.md) also includes the growing
crowding tail and native lifecycle tests. That tail changes the synthetic
changing-depth total below to approximately $9,385.71; the fixed-$50k example
is unchanged. Neither this study nor the lifecycle tests establish loss safety.

## Verdict

The incremental formula removes the illustrated opening-equity advantage at a
fixed market state. The opening-only shortcut is insufficient. Two additional
rules are necessary: retain each position's admission requirement for later
equity withdrawals/debt increases, and include entry-induced depth changes in
the before/after calculation. A candidate with these rules passes the arithmetic
tests below. Native integration was outside this study's scope; see the later
implementation contract for its coverage and remaining calibration work.

This equalizes the tested **entry capital**, not maintenance or liquidation
behavior. Three smaller positions still have different maintenance requirements
from one large position under the selected position-size tiers.

## The example, reproduced exactly

With $50,000 of fixed side depth, constant prices and no prior positions, define
`F(E, L)` by marginal initial-equity rates of 10%, 15%, and 20% at boundaries of
5% and 15% of side depth. These are the proposed 7/12/17% MM bands plus 3 points,
used here as an illustrative cumulative market-capital curve.

| Entry | Additional equity requirement | Total after entry |
| --- | ---: | ---: |
| First $15,000 | $2,500 | $2,500 |
| Next $15,000 | $3,000 | $5,500 |
| Next $15,000 | $3,000 | $8,500 |
| Alternatively: one $45,000 | $8,500 | $8,500 |

The result is independent of wallet identity. With fixed depth,
`F(E_after, L) - F(E_before, L)` telescopes across partitions. Rounding each
additional charge up cannot make splitting cheaper than rounding the combined
charge up. This does not select the high-exposure tail of the margin curve:
flattening its marginal rate at 20% is not sufficient calibration of unrestricted
market exposure.

## Two shortcuts that fail

### 1. Checking the formula only when collateral exposure increases

Dusk's `remove_leverage_margin` increases debt without increasing collateral.
An exposure-only delta is therefore zero. If the resulting position were checked
only against its own-size initial margin, each $15,000 position could retain
$2,500. The two later entries could withdraw $500 each, undoing $1,000 of the
new protection. The prior endpoint-crowding proposal would retain $7,550; that
was a different rule, with a $950 gap relative to the single position.

**Candidate remedy:** record a minimum admission-equity obligation per position,
in collateral units, with conservative rounding. Add the relevant incremental
charge on every collateral increase, including collateral supplied from the
owner's wallet. The latter already contributes equity, but must not create an
uncharged route to later debt extraction. On equity extraction or new debt,
require the whole resulting position to retain this obligation in addition to
the other applicable admission and cash checks. Surplus equity may fund an
increase; it must not be counted twice. Do not store an unchanging dollar amount.

### 2. Using the post-entry depth for both sides of the subtraction

Depth can change during entry. For a synthetic path with side depth
`L = 80,000 - aggregate exposure`, the market finishes at $45,000 exposure and
$35,000 depth:

| Calculation | First $15,000 | Second | Third | Total |
| --- | ---: | ---: | ---: | ---: |
| Both terms use post-entry depth | $2,350 | $3,000 | $3,000 | $8,350 |
| Full before/after states | $2,350 | $3,150 | $3,150 | $8,650 |
| One $45,000 at its final depth | — | — | — | $8,650 |

**Candidate remedy:** evaluate `F(E_after, L_after) - F(E_before, L_before)`
for the operation, in one consistent unit of account. The opening example then
matches. This is a controlled depth-path example, not a claim that Dusk's actual
concentrated AMM follows this path. Current native calibration observes depth
after entry, so this detail cannot be omitted from native integration tests.

If the potential falls during an operation, its delta must not become a negative
margin charge or withdrawable credit. Such cases need the independent resulting
position requirements and native state-transition tests; the positive-delta
synthetic paths here do not establish that complete rule.

## Lifecycle behavior and UX

The candidate bookkeeping model retains the entry obligation and releases at
most a proportional share when collateral is sold. It rounds new charges up and
partial releases down. Full closure clears the obligation. Pure repayment and
risk reduction must remain available without restoring initial margin; ordinary
maintenance and execution checks still apply. An admission obligation is not a
new liquidation threshold and is not proof that actual equity always exceeds IM.

At fixed price/depth, retained obligations cover the cumulative requirement in
all tested entry, increase, partial-close, full-close and reopen sequences.
Splitting plus proportional closure did not lower the combined requirement below
the curve in these scenarios. Existing Dusk partial close sells proportional
collateral and repays proportional debt; its actual fees and rounding still need
instruction-level tests against the new obligation bookkeeping.

**Selected UX:** the user accepted retaining the entry requirement for
predictability. The later $15,000 position retains its $3,000 admission requirement
even if the earlier traders exit. A fresh $15,000 position in an empty market
would need $2,500. The existing user can reduce/close, and fresh admission is
repriced on reopening, with the usual execution costs. Automatically refunding
the difference is an additional release policy not validated here. Other
traders' entries do not change existing maintenance or directly trigger
liquidation. Repaying debt can create withdrawable surplus above the retained
obligation.

External liquidity changes also prevent a universal history-independent result.
For example, $45,000 exposure admitted with $1,000,000 side depth requires $4,500
on this curve. After depth falls to $50,000, a new otherwise identical position
requires $8,500. Preserving old maintenance terms was selected product behavior;
this study does not show that the old position now holds the new requirement.
Conservative depth observations remain essential to resist temporary liquidity
inflation. Changing prices and depth must not be advertised as identical states.

## Checks actually run

Run [the exact-arithmetic script](check_incremental_crowding.py):

```sh
PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/check_incremental_crowding.py
```

- 2,000 arbitrary partition cases, including conservative integer charge rounding.
- 1,000 deterministic lifecycle paths, each with 100 modeled actions: fixed depth
  or the synthetic changing-depth path, exact arithmetic or integer charge/release
  rounding. Includes full exits followed by reopening and increases of existing
  positions. The asserted invariant concerns retained capital obligations.
- Explicit counterexamples for opening-only withdrawal checks and same-post-depth
  subtraction, plus the external-liquidity history limitation.

All checks pass. No native Rust/SBF build was run for this study. The existing
1080-case native parameter results test the prior endpoint-crowding proposal,
**not this incremental candidate**. Concentration, controller/hLP effects, both
debt directions, fees, price changes, potential decreases, temporary LP funding,
flash sessions and actual loss behavior remain native validation requirements.
No runtime settings changed; no commit or push was made for this study.
