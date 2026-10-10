# Leverage recovery available at emergency access

> Historical calibration: assumptions and proposed settings below are superseded
> by [the current liquidation decisions](../LIQUIDATION_DECISIONS.md). These saved
> results do not validate the new combined runtime policy. Reproduce historical
> numbers using the source revision recorded with the experiment.

2026-10-09. PR #45 source `58b02fa` plus local test-only calibration adapters. No runtime policy or default changed. The user authorized keeping the existing IM/MM curve and calibrating emergency access against remaining execution recovery.

## Recommendation for the next implementation decision

**Use 75% of stored effective MM as the proposed initial emergency boundary.** At 7% MM this is 5.25% symmetric-EMA equity. Keep the existing 10% small-position IM, 7/12/17% marginal maintenance bands, +3-point entry buffer and incremental crowding. Keep ordinary fixed-payment partial purchases available from MM; they may execute through Dusk, external venues or buyer inventory. Emergency permission remains based on EMA health, never solely on a manipulable spot quote. No timer or withdrawn solvent full-close shortcut is introduced.

This changes the previous two-thirds recommendation because the user's latest priority is preserving execution headroom. Among the three tested boundaries, 75% generally allows action with more remaining recovery. It is not optimal on every path: the [matched comparison](EMERGENCY_THRESHOLD_RESULTS.md) has 454 cases with lower realized shortfall than 50%, 164 with higher shortfall, and 150 ties. Earlier forced full sales can interrupt beneficial later partial fills. This is a product tradeoff pending selection, not proof that 75% is universally safe.

**The 4–5% buffer is conditional, not a guarantee.** In many stress checkpoints a hypothetical full Dusk sale already recovers less than debt at ordinary eligibility. Changing only the emergency boundary cannot create missing liquidity. That quote does not establish that partial purchases or external execution cannot repay the debt. Insurance and write-off are excluded from recovery measurements below.

## What the existing margins imply under a fixed haircut

For unchanged collateral quantity, fixed debt D, reference value C, equity h = 1-D/C and a constant all-in execution haircut k, the additional reference decline to break-even is `1 - (1-h)/(1-k)`. A negative result means the assumed sale already falls short. IM governs admission; these calculations begin at MM or the emergency boundary. Growing incentives, interest and changing slippage invalidate a constant haircut forecast; these numbers are arithmetic sensitivity only.

| Small-position stage | EMA equity | Further decline with 2% all-in haircut | With 3% | With 5% |
| --- | ---: | ---: | ---: | ---: |
| Ordinary liquidation | 7.00% | 5.10% | 4.12% | 2.11% |
| 75% emergency | 5.25% | 3.32% | 2.32% | 0.26% |
| Two-thirds emergency | 4.67% | 2.72% | 1.72% | Already short |
| 50% emergency | 3.50% | 1.53% | 0.52% | Already short |

At 30% of stored side depth, the retained curve gives approximately 13.67% effective MM and 16.67% IM before additional crowding (6x reference leverage). 75% emergency permission would open at 10.25% EMA equity. The higher reference cushion is not evidence of low execution impact.

## Native checkpoint evidence

This is a leverage-position experiment. Both debt directions are covered, but borrowing accounts, their collateral combinations and liquidation thresholds are not calibrated by this run. Shared flash accounting and borrowing-specific pricing still need their own instruction-level coverage.

The same 2,304 runs produce 24,870 position checkpoints. All original aggregate rows are byte-value equivalent after CSV parsing: adding quotes did not change execution outcomes. Admission, curve, concentration, controller, hLP, both debt directions, fees, withdrawals and paths are unchanged. See the [original experiment](EMERGENCY_THRESHOLD_RESULTS.md) for the full fixture.

Measure the first observed ordinary eligibility and the first observed emergency permission for each admitted position, before choosing that observation's fill. A position already closed by an ordinary fill has no later emergency checkpoint. Within an observation, preceding positions may already have changed the market. Consequently checkpoint populations differ and counts below are descriptive, not a matched causal comparison or empirical probability. Discrete observations can overshoot the nominal threshold; no claim of exact continuous triggering.

Net full-sale recovery is native swap output less the provisional progressive 0–1% caller reward and the solvent-only capped insurance contribution. Native swap fees and collateral transfer fees are already included. Repayment nets principal internally; only real outflows transfer debt tokens. The health reference column values collateral after its modeled transfer fee, matching the eligibility adapter. Accrued interest is frozen, so debt equals principal. Neither the 1% reward cap nor the 0.2% contribution has been selected as a runtime default. No keeper-profit gate is applied to observation quotes.

| Boundary | Checkpoint | Valid quotes | Full sale covers debt | Median recovery / debt |
| --- | --- | ---: | ---: | ---: |
| 75% | First ordinary eligibility | 2910 | 608 | 81.70% |
| 75% | First emergency permission | 2872 | 464 | 79.23% |
| Two-thirds | First ordinary eligibility | 2918 | 608 | 81.75% |
| Two-thirds | First emergency permission | 2806 | 444 | 77.83% |
| 50% | First ordinary eligibility | 2914 | 628 | 81.60% |
| 50% | First emergency permission | 2646 | 304 | 76.22% |

Quote failures: 0. A covering quote is not proof that every required token transfer, cash reservation or future transaction will execute. It does not authorize a full sale outside the agreed health and settlement rules.

### Concrete concentrated-pool checkpoint

First admitted position in the amplification-4/controller-on/hLP-on quote-debt fixture, ten requested 1,500-spend positions at 2x, slow decline, no token fees or LP withdrawal, ordinary-first ordering. Six positions are admitted. Values below are debt-token units; both checkpoint quotes refer to the 75% policy run.

| Checkpoint | EMA equity | EMA collateral value | Debt | Net full Dusk sale | Shortfall before insurance |
| --- | ---: | ---: | ---: | ---: | ---: |
| ordinary_eligible | 6.78% | 804.593 | 750.000 | 621.182 | 128.818 |
| emergency_permitted | 4.89% | 788.622 | 750.000 | 607.336 | 142.664 |

This illustrates the limit of the earlier constant 2–3% haircut example. It is not a new liquidation trigger and does not justify marking a position unhealthy solely because AMM liquidity falls.

## Implementation handoff and unresolved product choices

1. Keep the implemented stored margin terms, aggregate crowding and symmetric-EMA eligibility. Do not restore the 2% unwind cap or change MM to satisfy a presumed universal loss buffer.
2. Select the proposed 75% boundary before wiring an emergency runtime default. Keep ordinary fixed-payment flash purchases and internal emergency sales as distinct permissions; keep both atomic and principal-first.
3. Implement the shared flash/session/payment/accounting contract with explicit parameters. Reward cap, insurance contribution and dust funding still need their previously requested decisions; illustrative fixture values are not approvals.
4. Preserve instruction-level acceptance gates: actual net credit, same-market routing/netting, LP and parameter composition, transfer fees, accrued interest, insurance limits and write-off. The native adapter does not close finding #287652.
5. Retain the known stalled-fill risk: a position can stabilize below MM, above emergency permission, with no profitable ordinary fill. Do not add a timer, unapproved subsidy or early full-sale shortcut to hide this risk.

## Reproduce

```sh
DUSK_EMERGENCY_THRESHOLD_SWEEP=1 CARGO_TARGET_DIR=/private/tmp/dusk-emergency-headroom-20261009 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_OPT_LEVEL=2 CARGO_INCREMENTAL=0 cargo test -p dusk native_emergency_threshold_comparison -- --nocapture > /private/tmp/dusk-emergency-headroom-20261009.log 2>&1
PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/report_emergency_headroom.py /private/tmp/dusk-emergency-headroom-20261009.log
```

Raw checkpoints: [CSV](native-emergency-headroom.csv). Temporary build products and the extraction log can be removed after validation.
