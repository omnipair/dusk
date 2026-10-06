# Stored leverage margins

Implementation in consolidated PR #45. This implements the agreed margin
lifecycle; the flash-liquidation and loss-waterfall redesign remains separate
unfinished work in that same PR. No deployment is performed by this change.

## Existing positions keep their terms

`LeveragePosition.margin_terms` stores a collateral-side liquidity reference,
the maintenance band boundaries and rates, the entry buffer, and the retained
admission-equity obligation. The obligation uses collateral atoms times NAD
(1e9), so it scales with the collateral price rather than freezing a dollar sum.

Default marginal maintenance rates are 7/12/17%, with boundaries at 5/15% of
stored side depth. Each rate applies only to its band. Initial margin is at least
effective MM plus 3 percentage points and at least the retained admission rate.
Rate divisions round up. Fractional band boundaries are retained exactly.

An unchanged position is not repriced when others trade, enter, exit, add LP
liquidity or withdraw it. Price movements and interest still change actual equity
and liquidation health. No initial-margin check becomes a liquidation trigger.

Increasing risk rechecks the resulting whole position. The reference can tighten
to current conservative depth on that action; it cannot improve merely because
temporary depth was added. Previously retained equity cannot be refunded by a
falling market potential. Pure debt repayments do not reprice terms or require
restoring initial health. A proportional close releases the same proportion of
the obligation, rounding the retained amount up; maintenance can fall as size
crosses the stored bands. Fully clearing debt removes its collateral from the
market exposure total, even if it awaits a separate owner withdrawal.

## Admission and market exposure

Gross collateral in every debt-bearing leverage position is tracked separately
for base and quote, across all owners/namespaces. Neither unsolicited custody
transfers nor changes to LP supply alter these counters. Entry/increase adds
measured collateral credit; sale/close/liquidation removes the full position
debit. Full repayment removes the outstanding exposure once. A later debt-free
withdrawal does not remove it a second time. Future flash sessions must leave
this exposure live until settlement, as specified in the flash contract.

Conservative side depth is the smaller of collateral cash and debt cash
converted at the symmetric reference, then scaled down if pessimistic depth
EMA is below current total curve depth. Curve amplification and receivables
cannot count as physical cash. Zero usable depth rejects new risk. The risk
observation prevents a same-slot LP addition from instantly increasing depth.

Let `S(E,L)` be the progressive 10/15/20% initial-equity amount for aggregate
collateral E at depth L. The admission potential keeps the earlier gentle
crowding curve as a growing tail:

```text
C(E,L) = E × (10% + max(0, E/L − 20%) / 10)
F(E,L) = max(S(E,L), C(E,L))
additional obligation = max(0, F(E_after,L_after) − F(E_before,L_before))
```

Both states are captured around the native transition, including its hLP and
reserve effects. Add that obligation to the existing position obligation, then
enforce its own size-based initial requirement too. This is a development
parameterization of the previously proposed crowding tail, not evidence of
optimal loss performance. Its effect differs from applying an endpoint rate to
each independent position. Unlike the old endpoint-rate cap, the cumulative
potential is not capped at 100%; a charge leaving no leverage capacity rejects
admission instead of becoming cheaper beyond the cap.

At fixed $50k side depth, one $45k exposure needs $8,500; three successive $15k
exposures retain $2,500/$3,000/$3,000. Market changes between entries can change
the result. Smaller positions still have different maintenance/liquidation
behavior, and this does not guarantee an aggregate equity floor after price
losses or external liquidity removal.

Admission checks the retained IM against executable exit equity and reference
equity, including directional EMA protection and the existing scheduled token
fee admission policy. The fixed 2% unwind-impact rejection is removed. The 20x
requested-multiplier ceiling remains an outer bound, not an executable quote.
Increasing debt without increasing collateral (margin extraction) is included;
its resulting debt and entry obligation are both checked.

## Liquidation and protection

Ordinary leverage eligibility uses symmetric-EMA equity alone against the
position's stored size-based maintenance rate. A favorable AMM quote cannot
override that eligibility, and poor execution alone cannot liquidate an
EMA-healthy position. The scheduled transfer-fee remedy remains in place.
Protection-order health uses that same reference and maintenance requirement,
with the existing conservative one-basis-point rounding buffer.

This commit does not replace leverage's full-sale implementation with partial
flash liquidation, nor does it resolve manipulation finding #287652. Those are
tracked in the remaining redesign. Actual execution still has existing cash and
accounting constraints; stored margin rates are not a guarantee of liquidity.

## Interfaces and verification

Market layout v2 adds two gross exposure counters to Debt. LeveragePosition adds
the margin-terms structure; opened/updated events carry it as well. Both Dusk and
delegate interfaces must be rebuilt together. The SDK exposes
`getStoredLeverageMargins(position)` and `dusk.get.leverageMargins(address)` for
saved IM/MM display. These helpers are not entry execution quotes. Account
layout changes require matching program/SDK/indexer consumers.

Native regression coverage includes both debt directions, amplification 1/4/10,
hLP on/off, controller on/off, unrelated positions, LP withdrawals, proportional
closure and full repayment. It also checks rejection of a retained-equity
withdrawal bypass and successful admission beyond the old 2% impact limit.
Historical calibration fixtures explicitly retain their old flat liquidation
terms so their old outputs do not silently become claims about this new policy.
New policy stress calibration and the flash instruction tests remain necessary
for the complete redesign.

### Local acceptance run, 2026-10-07

The complete repository CI sequence passed: formatting, hygiene, code shape,
Clippy, 467 Dusk tests in each default/production profile, production checking,
16 delegate tests, one faucet test, TypeScript, all Anchor/SBF fixture builds,
clean build identity, regenerated IDL checks, 56 SDK tests, and 123 LiteSVM tests
with no pending tests and the complete compute baseline required. The benchmark
feature also compiles. The transaction regression confirms a rejected opening
rolls back market/exposure accounting and token balances. These checks validate
this implementation increment; they do not complete the remaining flash design,
loss-path calibration, combined audit or rollout.
