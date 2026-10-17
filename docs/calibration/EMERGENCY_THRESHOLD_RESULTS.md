# Emergency threshold comparison

> Historical calibration: assumptions and proposed settings below are superseded
> by [the current liquidation decisions](../LIQUIDATION_DECISIONS.md). These saved
> results do not validate the new combined runtime policy. Reproduce historical
> numbers using the source revision recorded with the experiment.

2026-10-08. Native economic experiment on PR #45 source `58b02fa`, plus the test-only adapter in this change. No runtime policy, instruction or default is changed.

## Initial recommendation, superseded on 2026-10-09

The [recovery checkpoint follow-up](EMERGENCY_HEADROOM_RESULTS.md) now recommends
75% of MM for the stated priority of preserving execution recovery, pending
selection. The comparison below remains unchanged evidence; neither boundary
guarantees a solvent sale or cleanup.

Use **two-thirds of MM as the starting emergency candidate**. This is a compromise between preserving ordinary partial-repair opportunities and intervening earlier on fast declines; the experiment does not establish a mathematically optimal threshold. 75% generally reduces realized shortfall sooner, but it also worsens some slow paths where later partial fills reduce the amount sold at once. 50% retains more partial fills and sometimes preserves borrower recovery, but leaves more eligible debt and performs worse on the tested fast declines.

**No candidate solves cleanup by itself.** A position can remain below MM but above the emergency boundary with no profitable ordinary Dusk fill, especially after the external target stabilizes. An incentive based only on health also stops increasing when health stops deteriorating. External routing may help, but is not guaranteed. Do not label this risk resolved or claim reliable cleanup from a passing sweep.

## Question and current design

Compare emergency full AMM-sale permission at 75%, two-thirds (rounded to 66.67%), and 50% of the position's stored maintenance requirement. At 7% MM these correspond to 5.25%, approximately 4.67%, and 3.50% symmetric-EMA equity. No timer and no early solvent full-close shortcut are used.

## Experiment

2,304 counterfactual runs: three thresholds × amplification 1/4/10 × controller off/on × hLP off/on × both debt directions × one/ten requested positions × four price paths × two fee/withdrawal fixtures × ordinary-first/emergency-first ordering. Each starts with 100,000 total wallet LP deposits and requests 15,000 total entry spending at 2×. Entry uses the current native open path, stored margin terms and incremental crowding; later entries can be rejected. Only identically admitted portfolios are compared.

Paths are a 0.5% decline per 60-second observation, a 3% decline per 10-second observation, a 45% gap followed by a flat external target, and a 0.6% decline for 60 observations followed by recovery. Price targets are reached through actual native trades in either direction, including arbitrage after liquidation. A step-2 stress fixture combines 25% yLP-share withdrawal with immutable 1% uncapped transfer fees on both tokens. The separate baseline has neither. This combination does not isolate their individual effects.

All ordinary modeled buyers route through Dusk. The model searches collateral slices on a 1% grid, accepts health-improving partials and targets remaining MM +2 points. Payment uses the EMA value of collateral delivered after its transfer fee, discounted by the selected 0.5–5% curve. Full fixed-payment purchases can have an authorized shortfall. The separate emergency can sell below that floor. Every full debt leg is cleared through recovery, capped insurance and native principal write-off.

Provisional economics held fixed: emergency reward 0–1% of proceeds as health falls from MM to zero; solvent insurance contribution 0.2%; keeper cost 0.05 debt tokens per fill; initial insurance 1,000 debt tokens with native draw limits. Keeper profitability uses actual net reward after the modeled outgoing transfer fee. Debt repayment is netted internally; only real outgoing profit, owner residual and insurance funding transfer debt tokens. Insurance transfer fees reduce the credit reaching repayment. Collateral custody-to-AMM transfer uses SPL fee rounding. Additional external-route/session-vault transfers are not modeled.

Emergency-first is a permitted-ordering stress case, not a prediction of keeper behavior. Ordinary-first cannot be claimed as an on-chain priority guarantee. Neither assumes keepers wait deliberately for a larger future reward.

## Coverage and exclusions

768 unique portfolios/path/ordering fixtures; 768 have admitted positions and no path/withdrawal error under all three thresholds. All rows satisfy original principal = sale repayment + insurance credit + write-off + remaining debt. Market reserve invariants and aggregate position-collateral exposure are checked after settlement. Rejected hypothetical operations commit no state.

| Threshold | No positions admitted | Path/withdrawal errors | Cash-rejected settlement attempts |
| --- | ---: | ---: | ---: |
| 75% | 0 | 0 | 0 |
| Two-thirds (66.67%) | 0 | 0 | 0 |
| 50% | 0 | 0 | 0 |

## Matched outcomes

Counts are deterministic stress cases, not probabilities. Shortfall is insurance credit plus principal write-off, so using insurance does not hide execution loss. Remaining debt is not counted as repaid. Values below are percentages of each run's original debt, then medians across matched fixtures; both debt directions are normalized separately rather than summing unlike tokens.

| Threshold | Median shortfall / initial debt | Median write-off / initial debt | Runs with eligible debt left | Emergency closes | Partial fills |
| --- | ---: | ---: | ---: | ---: | ---: |
| 75% | 24.668% | 19.667% | 14 | 2758 | 1054 |
| Two-thirds (66.67%) | 25.272% | 20.157% | 28 | 2642 | 2186 |
| 50% | 26.276% | 20.261% | 58 | 2404 | 3016 |

### Pairwise comparison

Each cell reports **lower / equal / higher** for the earlier boundary versus the later one. Owner value = realized owner residual + positive EMA equity of surviving positions at the final observation; it is marked value, not a guaranteed executable exit.

| Earlier vs later | Matched runs | Realized shortfall | Including quoted eligible-residual deficit | Write-off | Owner value |
| --- | ---: | --- | --- | --- | --- |
| 75% vs Two-thirds (66.67%) | 768 | 380 / 278 / 110 | 384 / 274 / 110 | 354 / 330 / 84 | 64 / 640 / 64 |
| 75% vs 50% | 768 | 454 / 150 / 164 | 464 / 148 / 156 | 416 / 216 / 136 | 100 / 588 / 80 |
| Two-thirds (66.67%) vs 50% | 768 | 430 / 208 / 130 | 440 / 206 / 122 | 394 / 266 / 108 | 70 / 620 / 78 |

### Break down 75% versus 50%

| Path | Ordering | Matched runs | Shortfall: lower / equal / higher | Owner value: lower / equal / higher |
| --- | --- | ---: | --- | --- |
| Slow decline | Ordinary first | 96 | 64 / 0 / 32 | 4 / 68 / 24 |
| Slow decline | Emergency first | 96 | 62 / 2 / 32 | 6 / 70 / 20 |
| Fast decline | Ordinary first | 96 | 96 / 0 / 0 | 0 / 96 / 0 |
| Fast decline | Emergency first | 96 | 96 / 0 / 0 | 0 / 96 / 0 |
| Gap | Ordinary first | 96 | 22 / 50 / 24 | 16 / 78 / 2 |
| Gap | Emergency first | 96 | 22 / 48 / 26 | 16 / 68 / 12 |
| Dip/recovery | Ordinary first | 96 | 50 / 22 / 24 | 26 / 57 / 13 |
| Dip/recovery | Emergency first | 96 | 42 / 28 / 26 | 32 / 55 / 9 |

## Representative concentrated case

Amplification 4, controller and hLP enabled, quote debt, ten requested 1,500-spend positions at 2×, slow decline, no transfer fees/withdrawals, ordinary-first. Amounts below are debt-token units.

| Threshold | Opened | Initial debt | Insurance credit | Write-off | Owner residual | Eligible debt left | First emergency equity / MM |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 75% | 6 | 4500.000 | 500.000 | 342.314 | 0.000 | 0.000 | 5.12% / 7.00% |
| Two-thirds (66.67%) | 6 | 4500.000 | 500.000 | 366.344 | 0.000 | 0.000 | 4.65% / 7.00% |
| 50% | 6 | 4500.000 | 500.000 | 413.977 | 0.000 | 0.000 | 3.20% / 7.00% |

### Counterexample: earlier full sale can recover less

Amplification 4, controller off, hLP off, quote debt, one 15,000-spend 2× position, slow decline, 25% LP-share withdrawal and 1% transfer fees, ordinary-first. All thresholds start with 7,500 debt tokens. Later permission allows additional partial fills as the path evolves, reducing the eventual sale size.

| Threshold | Partial fills | Emergency closes | Insurance credit | Write-off | Remaining debt |
| --- | ---: | ---: | ---: | ---: | ---: |
| 75% | 2 | 1 | 198.095 | 854.076 | 0.000 |
| Two-thirds (66.67%) | 4 | 1 | 198.533 | 710.380 | 0.000 |
| 50% | 6 | 0 | 124.895 | 0.000 | 0.000 |

## Eligible residuals

The extra comparison above adds the final full-sale quote deficit (after the same emergency reward) on remaining eligible positions to already realized shortfall. This is an indicative reserve quote, not permission or proof that the liquidation can execute. Unquoteable residuals are reported separately.

| Threshold | Eligible positions | Above emergency boundary | Largest eligible debt | Residual quote failures |
| --- | ---: | ---: | ---: | ---: |
| 75% | 16 | 6 | 750.000 debt tokens | 0 |
| Two-thirds (66.67%) | 36 | 14 | 750.000 debt tokens | 0 |
| 50% | 92 | 42 | 750.000 debt tokens | 0 |

## Limits

- No external liquidity/buyer inventory is assumed. Better outside execution could repair positions before any emergency, or make an early forced close avoidable.
- Accrual is frozen. These results do not validate principal-first allocation of accrued interest, canceled interest, referral fees or production flash accounting.
- Fixed 2× requested openings, one initial LP scale, one insurance seed, a discrete fill grid and four deterministic paths do not establish optimal parameters or loss probabilities.
- Permissionless keeper competition is assumed; no latency, transaction contention, strategic waiting, manipulation or adversarial LP activity is simulated.
- This is a native economic adapter. Explicit sessions, reserve-credit verification, token CPIs, shared-state composition and finding #287652 still require implementation/audit.

## Reproduce

```sh
DUSK_EMERGENCY_THRESHOLD_SWEEP=1 CARGO_TARGET_DIR=/private/tmp/dusk-emergency-threshold-20261008 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_OPT_LEVEL=2 CARGO_INCREMENTAL=0 cargo test -p dusk native_emergency_threshold_comparison -- --nocapture > /private/tmp/dusk-emergency-threshold-20261008-full.log 2>&1
PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/report_emergency_thresholds.py /private/tmp/dusk-emergency-threshold-20261008-full.log
```

The focused normal test runs three thresholds on one concentrated/hLP fixture. The complete report requires all 2,304 rows and a passing test. Temporary build artifacts can be removed after extraction; the CSV and report preserve results.
