# Native parameter comparison

2026-10-07. Calibration evidence only; runtime defaults are unchanged. See [the recommendation](PARAMETER_RECOMMENDATION.md) for pending product decisions.

## Scope

1,080 counterfactual runs: five policies × amplification 1/4/10 × controller off/on × hLP off/on × one/ten requested positions × slow/fast/gap decline × wallet funding 16.67%/20%/50% (approximately 6×/5×/2× requested entry leverage). Quote-token debt, 100,000 initial wallet liquidity, 15,000 total requested entry spending, no withdrawals. The original 576-case comparison separately covers both debt directions and withdrawals.

The baseline uses 7/12/17% marginal MM at 5/15% stored side depth, IM = max(MM +3 points, 10% + one point per ten points of crowding above 20%), recovery MM +2 points, 0.5–5% buyer discount, 2% solvent-close window, critical health at half MM, 0–1% internal caller reward, 0.2% insurance contribution and 0.05 keeper cost. The caller reward scales linearly with EMA distress and is capped at zero equity.

Controller-on also enables the configured divergence fee; it is a combined fixture change, not an isolated measurement of controller effects. This experiment retains the original native engine, hLP accounting, curve, cash checks and insurance caps. It freezes accrual, has no token transfer fees, uses a 1% collateral search grid and models purchases executed inside Dusk only. No flash instruction or external route is tested. Keeper entry is assumed whenever the modeled net reward covers cost.

## Admission

| Requested leverage | Runs | No entry admitted | Runs with at least one entry |
| --- | ---: | ---: | ---: |
| ≈6× | 72 | 36 | 36 |
| 5× | 72 | 36 | 36 |
| 2× | 72 | 0 | 72 |

All single 15,000-spend entries requested at 5× or approximately 6× fail this adapter's admission checks. Splitting into 1,500-spend requests admits some positions before later requests fail. This does not meet a promise of 6× wallet leverage for a large position. The selected 6× target concerns reference margin at 30% stored depth; AMM impact, fees and reference/exit equity still constrain entry.

## Matched policy changes

Only runs with admitted positions and identical initial debt enter this comparison. Counts are deterministic stress fixtures, not market probabilities or expected returns. Compare insurance consumption and residual debt too: write-off alone understates loss.

| Change from baseline | Matched runs | Lower write-off | Same | Higher | Admission count changed |
| --- | ---: | ---: | ---: | ---: | ---: |
| Critical EMA boundary: 75% of MM | 144 | 123 | 21 | 0 | 0 |
| Internal reward cap: 0.25% | 144 | 129 | 14 | 1 | 0 |
| Recovery target: MM +1 point | 144 | 0 | 144 | 0 | 0 |
| Twice the crowding slope | 144 | 0 | 144 | 0 | 0 |

The earlier emergency's result includes the smaller reward at lower distress, not just a different sale price. It permits an earlier full forced close. Smaller rewards mechanically retain more sale proceeds when execution occurs; this model cannot establish that real keepers would act as quickly at the lower reward. The crowding comparison did not change admission on these fixtures, so it does not identify an optimal crowding slope. Recovery settings alter fill counts and insurance use even where final write-off is unchanged.

## Representative slow-decline outcomes

Both controller settings below use amplification 4, active hLP, ten requested 2× entries. Amounts are debt-token units. All three policies admit the same positions for each controller setting. No retained debt is counted as recovered.

| Controller | Policy | Opened | Initial debt | Partial fills | Solvent closes | Emergency closes | Insurance | Write-off | Remaining debt |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| false | baseline | 10 | 7500.00 | 42 | 6 | 2 | 21.694 | 0.000 | 0.146 |
| true | baseline | 7 | 5250.00 | 0 | 0 | 7 | 500.000 | 563.697 | 0.000 |
| false | critical_75 | 10 | 7500.00 | 29 | 6 | 4 | 28.346 | 0.000 | 0.000 |
| true | critical_75 | 7 | 5250.00 | 0 | 0 | 7 | 500.000 | 473.938 | 0.000 |
| false | reward_025 | 10 | 7500.00 | 23 | 5 | 3 | 21.792 | 0.000 | 0.000 |
| true | reward_025 | 7 | 5250.00 | 0 | 0 | 7 | 500.000 | 547.121 | 0.000 |

## Residual-cost diagnosis

Replayed the 48 slow/controller-off combinations covering every unfinished run in the prior 576-case sweep. 22 runs retain 32 eligible positions. All have an allowed fill when the same final state is evaluated with zero keeper cost, and no selected fill at the modeled 0.05 cost. This isolates an economic cleanup obstacle in those final states. It does not simulate a subsidy or prove that every possible size/route fails at nonzero cost.

Total remaining debt per affected run ranges from 0.146 to 2.605 debt tokens. Some remaining positions are healthy; the eligible-debt column separates them. A progressive percentage reward remains too small for sufficiently tiny balances. No unconditional cleanup guarantee follows from keeper competition.

## Reproduce

```sh
DUSK_PARAMETER_SWEEP=1 DUSK_RESIDUAL_SWEEP=1 cargo test -p dusk concentrated_cushion::parameters -- --nocapture > /private/tmp/dusk-parameters.log 2>&1
PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/report_parameters.py /private/tmp/dusk-parameters.log
```

Normal CI runs three representative parameter cases plus two residual cases. The full sweep validates conservation and surfaces unexpected admission, execution or accounting failures. The report requires a complete passing log. These economic tests do not replace runtime integration, adversarial instruction tests or validation of principal-first loss accounting with accrued interest.
