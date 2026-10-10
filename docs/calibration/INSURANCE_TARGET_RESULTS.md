# Insurance target sensitivity — 2026-10-10

## Recommendation

The user approved 5% of outstanding covered borrower/leverage principal per debt token as the provisional calibration target. Zero-insurance launches and optional pre-funding are approved; the illustrated seed is not required. The user selected an additional one-fifteenth of indexed hLP funding debt in the same token; the funding tables below exclude that component and hLP claims. This is an operating budget, not a statistically established optimum or a claim that it covers the saved stress scenarios. The user also approved full designated funding through 75% of target, then a linear taper to zero at the target.

## Selected mechanics and illustrative assumptions

Selected: 1% fee on net debt repaid; 0.2% to protocol and 0.8% to insurance/LPs; up to 75% principal-loss coverage under a shared 50% fund budget per 24 hours.

Funding illustration: constant 100,000 covered principal, 1,000 seed, no losses, no transfer fees, fully collected solvent fees, 30-day months and daily credits. Tested monthly repayment bases are assumptions, not volume estimates. The main comparison uses full designated funding until 75% of target, then a linear taper to zero. Knee sensitivities at 50% and 90% are also computed. No existing fund is paid out when its target declines.

## Fully funded capacity

No inflows, no previous window spending, no competing hLP claims and no token transfer fees. To cover 75% of a loss L within a 50% draw budget requires fund >= 1.5 × L. Capacity depends on actual balance, not the configured target.

| Target / principal | Fund | Window budget | Loss / principal receiving full 75% coverage |
| ---: | ---: | ---: | ---: |
| 2% | 2,000 | 1,000 | 1.33% |
| 5% | 5,000 | 2,500 | 3.33% |
| 10% | 10,000 | 5,000 | 6.67% |

## Months to 95% of target

| Target | Monthly repayment = 10% of principal | 50% | 100% |
| ---: | ---: | ---: | ---: |
| 2% | 16.3 | 3.3 | 1.6 |
| 5% | 59.5 | 11.9 | 5.9 |
| 10% | 131.5 | 26.3 | 13.1 |

At the 5% target and 100% monthly repayment base, moving the taper knee to 50% / 75% / 90% reaches 95% of target in 9.03 / 5.93 / 4.80 months.

While fully underfunded, replenishment is at most 0.008 × eligible net repayment volume. Covering 75% of losses therefore requires losses <= 1.067% of that repayment volume for fee inflows alone to keep pace, before hLP claims and costs. Near target the taper reduces funding further. Large initial claims require seed capital.

## Existing stress evidence

2,304 saved rows represent 768 matched fixtures at three counterfactual thresholds. They are deliberately chosen stresses, not independent observations or an empirical loss-frequency distribution. Each ratio uses that scenario's admitted original debt; this is not total market TVL. Realized principal shortfall before insurance equals insurance credited + principal written off. Unsettled debt is excluded from that realized measure and is not treated as recovered. 912 rows record a later requested entry rejected by initial-margin checks; admitted debt, not requested exposure, is the denominator.

| Historical threshold / MM | Median shortfall / original debt | P90 (fixture grid) | Max | Cases still eligible |
| ---: | ---: | ---: | ---: | ---: |
| 75.00% | 24.67% | 41.01% | 57.05% | 14 / 768 |
| 66.67% | 25.27% | 41.73% | 57.05% | 28 / 768 |
| 50.00% | 26.28% | 43.21% | 57.05% | 58 / 768 |

These median shortfalls would require roughly 37–40% of original debt in insurance to cover 75% within one 50% draw window, if realized in that window. Even 10% does not cover these median stresses at the intended 75% rate. That is a conditional sizing check, not a measured 24-hour risk estimate.

The fixtures use the earlier full-emergency / health-only incentive / 0.2% fee design, with frozen interest and no external routes. They do not validate the selected 70%-of-MM, 120-second incentive, partial-first, 1%-fee redesign. hLP affects native pool behavior, but these loss columns do not aggregate hLP insurance claims. A principal-only target therefore needs an additional hLP allowance.

## Validation and next decision

Checked unique and matched fixture keys, 768 rows per threshold, positive debt, only the expected entry-rejection label, and principal conservation in all 2,304 rows. All 27 funding cases conserve seed + fees = fund + LP allocations and respect target bounds. Capacity formulas reconcile with hand-calculated 75% / 50% limits. This Python analysis is not an instruction-level implementation test.

No unique optimum can be estimated without an accepted uncovered-loss objective, loss-frequency data, solvent repayment volume, correlated hLP claims and seed policy. Carry the selected provisional target into integrated calibration; do not describe it as a release-ready insurance guarantee.

Executed: 2026-10-10T11:01:19.070067+00:00. Input SHA-256: `e480d1f41495dd749f091087a3b57def9f8a48e3eef5ed37a611e449c99d0557`.

Sources: `LIQUIDATION_DECISIONS.md`, `insurance_policy_analysis.py`, `native-emergency-thresholds.csv`, `EMERGENCY_THRESHOLD_RESULTS.md`. Run `PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/insurance_target_analysis.py --write` to regenerate this report and print the complete result JSON.
