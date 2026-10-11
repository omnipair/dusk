# Emergency AMM recovery fallback

Approved 2026-10-12 for PR #45. This follows the
[unfilled liquidation investigation](UNFILLED_LIQUIDATIONS.md), whose CSVs retain
the baseline before this policy change.

## Selected rule

Full normal swap fees remain in force. Inside the unchanged symmetric-EMA
emergency boundary (equity at or below 70% of MM), permit a full AMM sale when
EMA equity is nonpositive or when no smaller AMM sale can restore the position
to MM + 2 percentage points after costs. Keep improving smaller fills available.

A partial that improves health without reaching the target, and an external
flash opportunity, no longer veto this AMM fallback. This is deliberate: it
allows a keeper to choose full closure even when a better external partial exists.
It is not a guarantee of best execution. Ordinary flash full-close permission
retains the stricter no-improving-partial condition. SDK callers must use the
preview corresponding to their intended execution path.

The certificate reuses the existing first-recovery search across all integer
collateral inputs, now bounded by the whole position for the full fallback.
Each interval uses optimistic repayment/value and maintenance bounds; individual
remaining AMM candidates use actual fees, reward and debt allocation. A possible
recovering partial, uncertain result, or exhausted work budget rejects full
closure. No failed keeper-selected request can authorize it. Full liquidation
still requires all the collateral; smaller calls retain positive debt shares.

## Replay results

The native matrix contains 576 configurations covering both debt directions,
1x/4x/10x concentration, hLP, transfer fees, LP withdrawals and three price paths.
288 admit no position; 16 admitted paths reach the stress-input search cap and
are excluded from the matched completed comparison. The remaining 272 admitted
paths are matched to the prior program (136 per execution assumption).

| Eligible positions at the original endpoint | Before | After |
| --- | ---: | ---: |
| Dusk-only route | 32 | 15 |
| Modeled external route | 4 | 2 |
| Inside the emergency boundary, either route assumption | 19 | 0 |

All 15 remaining Dusk-only cases are above the emergency boundary. They also
remain eligible after the unchanged 20-minute continuation. The two remaining
external cases become eligible at the last original sample and recover in the
extension. Completed native settlements and market invariants pass. The benchmark
now asserts that these completed stress paths leave no emergency-eligible
residual; this is a regression for these fixtures, not a universal liveness proof.

### Earlier sale changes realized losses

| Matched completed paths | Less principal write-off | More | Same |
| --- | ---: | ---: | ---: |
| Dusk only | 94 | 17 | 25 |
| Modeled external route | 88 | 18 | 30 |

These counts are not probabilities or a calibration of live market behavior.
Some increases realize losses on positions previously left open; the comparison
does not mark remaining risky debt as already lost. Earlier closure can also
prevent further deterioration along the prescribed paths. All comparisons are
within matching scenarios and debt-token units, not sums across different assets.
Collected swap fees remain LP/protocol assets, including retained collateral,
so principal write-off alone is not total LP profit or loss.

The external venue remains hypothetical: 100,000 debt-token CPMM depth reset per
quote, 30-bps venue fee, plus transfer fees. The finite size search and 50-atom
keeper-cost assumption do not prove route availability or optimality. The stress
helper only pushes overpriced Dusk liquidity down to the mark; it does not model
arbitrage restoring a price pushed below it. Extended terminal-position runs are
separate clones, not a single continued market simulation.

## Protection and integration tests

- Exhaustive small-integer scenarios refuse a full AMM quote whenever a permitted
  AMM partial can recover, across maintenance tiers, fee modes and token-fee caps.
- A useful flash partial can coexist with a permitted full AMM fallback for both
  borrowing and leverage in both debt directions. The ordinary full flash quote
  remains rejected. Full output matches the ordinary fee-bearing AMM quote.
- The existing first-recovery test now also rejects a full sale when an actual
  partial can restore health.
- A LiteSVM transaction uses the SDK's ordinary preview and emergency builder:
  above-boundary full execution rejects; positive-equity emergency fallback
  clears the position. The compute regression retains a 1.25M-CU ceiling.

All required local CI checks pass: formatting, repository hygiene, code shape,
Clippy, 494 Dusk tests in each of the normal and production builds, 16 delegate
tests, one faucet test, TypeScript, all SBF builds, build identity, IDL preparation,
and the SDK package checks. The strict LiteSVM suite passes all 134 tests with no
pending tests and the complete compute baseline enforced. The full-fallback
transaction consumes 266,900 CU, below its existing 1.25M-CU ceiling; SDK preview
and execution agree on output, reward and principal write-off.

The additional 526-test benchmark suite and 576-configuration diagnostic replay
pass. Regenerated SDK interfaces are unchanged: no account, argument, return or
event layout change is required. The SDK uses on-chain previews; its docs describe
the distinct full-sale permissions. These are local results before pushing this
commit; GitHub CI remains a separate gate.

## Retained risk and rollout

Original finding #287652 remains an execution-price risk. This policy explicitly
accepts emergency AMM execution below the ordinary flash floor; it does not prove
manipulation unprofitable. The EMA gate is preserved, and price/fee/liquidity
changes can still affect recovery feasibility and sale proceeds once inside it.
No new fee waiver, timer, insurance subsidy, debt minimum or deployment is part
of this change. PR #45 remains draft pending its wider review and rollout work.

## Reproduce

```sh
DUSK_LIQUIDATION_DIAGNOSTICS=1 cargo test -p dusk --lib --features benchmark final_economic_policy_paths -- --nocapture
```

- [All current paths](emergency-fallback-paths.csv)
- [Matched before/after comparison](emergency-fallback-comparison.csv)
- [Current eligible terminal states](emergency-fallback-states.csv)
- [Current continuations](emergency-fallback-continuations.csv)

Money in these CSVs uses 1,000 raw atoms per modeled token. Scenario identifiers
follow the original report. The diagnostic zero-divergence continuation is a
historical counterfactual only; no fee exemption was implemented or approved.
