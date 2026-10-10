# Native concentration and hLP repayment-cushion comparison

2026-10-05. This extends the earlier plain-CPMM economic comparison with Dusk's actual integer concentrated curve, fee accounting, recentering, EMA, repayment and hLP transitions. The proposed eligibility/reward/partial policy is a test adapter; flash instructions are still unimplemented.

## What is actually exercised

- 576 scenarios: peak amplification 1/4/10, controller off/on, hLP off/on, both debt directions, 1/10 requested positions, slow/fast/gap paths, solvent window off/2%, and no/25% yLP withdrawal.
- Concentration uses the native core, shoulders and nonzero tails. Amplified fixtures use `core_half_width_bps=100`, `fade_width_bps=400`. Available cash is never multiplied by amplification.
- Enabled controller: 60-second center EMA, 1% adjustment threshold, 0.1% step, one-slot minimum interval, divergence-fee coefficient `10 * NAD`. Disabled fixtures have no center adjustment or divergence surcharge. These are explicit comparison configurations, not proposed launch defaults. Actual funded center moves and deferred targets are counted; no manual reserve injection funds them.
- Native single-sided entry activates both hLP vaults at 2x target leverage. Total initial wallet deposits remain 100,000 tokens at the initial 1:1 mark: either 100,000 yLP deposits, or 80,000 yLP + 10,000 per hLP asset. Insurance has a separate 1,000 debt-token balance and native event/day caps.
- Successful settlements check native market invariants, zero hLP residual exposure, and debt conservation: original principal = proceeds repayment + insurance + write-off + remaining principal. Solvent-mode settlement forbids insurance and write-off. Four seeded settlement anchors separately cover solvent and loss-taking exits with active hLP in both debt directions.

## Matched examples

Quote-token debt, ten positions requested, slow path, no withdrawal. The 15,000 requested entry spend is 50% wallet-funded. Actual admission counts matter: changing native fees/curve/hLP cash can admit fewer positions. Amounts below are debt-token units. Remaining debt is not counted as recovered.

| Amp | Controller | hLP | Window | Opened | Starting debt | Solvent closes | Write-off | Insurance | Remaining debt | Eligible left | Recorded limitation |
| ---: | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 1 | false | false | off | 10 | 7,500.00 | 0 | 0.00 | 0.58 | 2.60 | 4 | — |
| 1 | false | false | 2% | 10 | 7,500.00 | 5 | 0.00 | 3.47 | 1.64 | 1 | — |
| 1 | false | true | off | 9 | 6,750.00 | 0 | 0.00 | 0.36 | 2.28 | 3 | entry_margin |
| 1 | false | true | 2% | 9 | 6,750.00 | 4 | 0.00 | 2.44 | 2.16 | 1 | entry_margin |
| 1 | true | false | off | 8 | 6,000.00 | 0 | 633.40 | 500.00 | 0.00 | 0 | entry_margin |
| 1 | true | false | 2% | 8 | 6,000.00 | 0 | 633.40 | 500.00 | 0.00 | 0 | entry_margin |
| 1 | true | true | off | 6 | 4,500.00 | 0 | 402.99 | 500.00 | 0.00 | 0 | entry_margin |
| 1 | true | true | 2% | 6 | 4,500.00 | 0 | 402.99 | 500.00 | 0.00 | 0 | entry_margin |
| 4 | false | false | off | 10 | 7,500.00 | 0 | 0.00 | 25.29 | 0.48 | 1 | — |
| 4 | false | false | 2% | 10 | 7,500.00 | 2 | 0.00 | 40.35 | 0.48 | 1 | — |
| 4 | false | true | off | 10 | 7,500.00 | 0 | 0.00 | 22.80 | 0.88 | 1 | — |
| 4 | false | true | 2% | 10 | 7,500.00 | 6 | 0.00 | 21.69 | 0.15 | 1 | — |
| 4 | true | false | off | 9 | 6,750.00 | 0 | 843.30 | 500.00 | 0.00 | 0 | entry_margin |
| 4 | true | false | 2% | 9 | 6,750.00 | 0 | 843.30 | 500.00 | 0.00 | 0 | entry_margin |
| 4 | true | true | off | 7 | 5,250.00 | 0 | 563.70 | 500.00 | 0.00 | 0 | entry_margin |
| 4 | true | true | 2% | 7 | 5,250.00 | 0 | 563.70 | 500.00 | 0.00 | 0 | entry_margin |
| 10 | false | false | off | 10 | 7,500.00 | 0 | 0.00 | 346.36 | 0.83 | 1 | — |
| 10 | false | false | 2% | 10 | 7,500.00 | 6 | 0.00 | 288.24 | 0.00 | 0 | — |
| 10 | false | true | off | 10 | 7,500.00 | 0 | 0.00 | 198.40 | 0.92 | 1 | — |
| 10 | false | true | 2% | 10 | 7,500.00 | 7 | 0.00 | 240.27 | 0.00 | 0 | — |
| 10 | true | false | off | 10 | 7,500.00 | 0 | 1,149.71 | 500.00 | 0.00 | 0 | — |
| 10 | true | false | 2% | 10 | 7,500.00 | 0 | 1,149.71 | 500.00 | 0.00 | 0 | — |
| 10 | true | true | off | 10 | 7,500.00 | 0 | 1,149.25 | 500.00 | 0.00 | 0 | — |
| 10 | true | true | 2% | 10 | 7,500.00 | 0 | 1,149.25 | 500.00 | 0.00 | 0 | — |

## Native limits and observed failures

0 paths stopped before completing the requested stress because a native countertrade/withdrawal could not be completed. 0 runs recorded failed full-sale execution attempts; 0 runs encountered the hLP reserve-reconciliation `BrokenInvariant` guard in either a stress trade or a full-sale attempt. 22 runs still have eligible positions at the finite horizon. Execution success alone does not select safe parameters or guarantee complete cleanup. A stopped stress path is neither a successful liquidation nor proof that all routes are unavailable.

| Recorded run limitation | Runs |
| --- | ---: |
| entry_margin | 120 |

Committed stressed observations include 2592 center changes, 2276 deferred-center observations and 23936 tail observations. These are across repeated counterfactual scenarios, not distinct real-world events or a success rate.

A focused cash-limit test seeds curve backing that includes a borrow receivable. A full sale quote succeeds but exceeds spendable reserve cash; actual native preparation rejects it with `InsufficientLiquidity`. A quoted positive cushion must therefore be paired with the exact execution cash policy and hLP funding floors. This fixture tests a constraint; it does not claim the seeded position passed admission.

### hLP reconciliation correction

The prior report at `6f85ee9` recorded 135 runs hitting the reserve-identity guard, 103 stopped stress paths and 43 runs with rejected full-sale attempts. The quote had replaced recorded starting hLP debt with a hypothetical freshly hedged claim after reserve/share changes. That refinance had never happened in the ledger. A minimal funded-recenter fixture with 10,000 tokens of actual debt on each side inferred about 10,333 and 10,667 instead, creating unexplained reserve discrepancies.

The native starting state now satisfies `ordinary + target equity + recorded indexed opposite-hLP debt = executable total reserve` on each side. It keeps the existing NAV valuation and reserves accrued funding interest exactly once; endpoint reconstruction no longer subtracts that interest again. Recenter funding projects the same actual post-deployment reserve state that execution will use. The three-atom reconciliation guard remains intact. First-entry price checkpoints include the backing already recorded before receipt tokens are minted, avoiding a transient pre-mint price. Regressions cover that sequence for both assets under CPMM and concentration, both swap directions, LP withdrawals, released protected inventory, funding cash conservation, and deliberate unexplained drift.

## Scope and interpretation

Native concentration changes execution and admission; it is not a constant liquidity multiplier. Trades can leave the core, and recentering can wait for its funding budget. A result at one amplification/width cannot select a universal repayment window. Partial fills remain preferred in this adapter; keeper ordering alternatives are in the earlier Python report, not this sweep.

The engine first executes actual entries, then actual countertrades toward the initial 1:1 mark; it never overwrites reserves to erase entry impact. Stress requests target 0.5% declines per minute, 3% per ten seconds, or a 45% gap then flat. The native price-search has a bounded input range and records failure rather than fabricating an unreachable price. Native liquidation sales may push the mark below that requested path; no restorative outside arbitrage is assumed. The horizon is 180 observations.

Policy candidates: 7/12/17% marginal MM above 5/15% stored side depth; IM >= MM +3 points plus crowding; 0.5–5% health discount; up to 1% internal reward; 0.2% insurance contribution; critical loss permission at half MM; partial recovery MM +2 points; fixed keeper cost 0.05. Apart from already recorded user selections, these are experiment inputs. Partial search uses 1% collateral increments and is not an optimal-seizure proof.

Interest accrual is deliberately frozen in this native economic sweep so native legacy proportional-interest allocation does not masquerade as the selected principal-first policy. The existing funding-interest, unpaid-interest and concentrated-loss tests remain separate evidence. Implementing the new principal-first waterfall with accrued interest is still required.

The adapter composes a real native spot sale, repayment, then native zero-credit final clearance if debt remains. It requires the gross spot output to be physically fundable. Existing `Close`/`Liquidate` policies already net debt internally and have different cash needs; the spot rejection does not prove those paths fail. The redesigned path needs its own proof and cannot bypass hLP funding. Insurance contributions are credited to native insurance state. No token CPI, transfer-fee token, flash-session lock, external venue, same-transaction LP/parameter concurrency or compute budget is validated by this adapter.

## Reproduce

```sh
DUSK_NATIVE_CUSHION_SWEEP=1 cargo test -p dusk native_concentrated_cushion_report -- --nocapture > /private/tmp/dusk-concentrated-cushion.log 2>&1
PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/report_native_cushion.py /private/tmp/dusk-concentrated-cushion.log
cargo test -p dusk native_cushion_
```

Replay one formerly rejected hLP path (4x, controller on, hLP on, quote debt, ten requested positions, slow path, 2% window, no withdrawal):

```sh
DUSK_NATIVE_CUSHION_CASE='4,true,true,1,10,0,200,0' cargo test -p dusk native_concentrated_cushion_report -- --nocapture
```

Normal CI runs eight representative paths plus the focused cash/hLP anchors. The explicit environment variable enables all 576 paths. The extractor requires a complete passing test log and checks every row's debt identity; test completion does not mean every economic scenario succeeded. Remove disposable build/log artifacts after saving the report.
