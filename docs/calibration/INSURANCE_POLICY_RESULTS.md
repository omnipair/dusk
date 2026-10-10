# Insurance funding and spending sensitivity

> Historical calibration: assumptions and proposed settings below are superseded
> by [the current liquidation decisions](../LIQUIDATION_DECISIONS.md). These saved
> results do not validate the new combined runtime policy. Reproduce historical
> numbers using the source revision recorded with the experiment.

2026-10-09 · PR #45 design analysis · 27 funding cases and 72 loss-budget cases. No runtime change. Values are hypothetical USDC amounts, with one USDC treated as $1. This is a small deterministic budget study, not actuarial calibration, a loss forecast, or a replay of the revised liquidation mechanism.

**2026-10-10 decision update:** the user selected a 1% total charge on net debt repaid, split as 0.8% to insurance/LPs and 0.2% to protocol. The user confirmed that the 0.8% allocation supplies insurance through the target-based taper and sends its remainder to LPs; the protocol allocation stays separate. Target and taper parameters remain unselected. The historical funding cases below do not model this split; neither the full 1% nor the protocol's 0.2% may be assumed to fund insurance. The ordinary buyer discount and emergency caller reward remain unchanged.

## Historical funding candidates and current spending direction

- Test a **5% target against covered borrowing/leverage principal per debt token**, plus a separately calibrated allowance for hLP claims sharing that vault and a minimum seed. Compare 2% and 10%; 5% is a middle capital allocation, not a proven sufficient buffer.
- Allocate all designated realized fee income to insurance until **75% of target**, then taper linearly to zero at target. Direct the remainder to the designated LP fee recipients; don't automatically distribute the existing fund when the target falls.
- The historical **0.2% secondary liquidation-levy candidate** was superseded by the selected 1% total charge and split above. The older assumed insurance fee flow replenishes slowly at the illustrative turnover below. Initial seed and any allocation from other realized fees require a budget/source decision.
- The user's latest spending candidate is **up to 75% of eligible losses, subject to a shared 50% fund budget**. The user confirmed that this shared budget refreshes every 24 hours against the remaining insurance balance. It replaces the existing 20%-of-fund per-event cap. LPs bear at least 25% of eligible borrower/leverage principal losses, and more when the shared cap binds. The user subsequently affirmed this spending direction: 75% principal-loss coverage within the 50% budget and 24-hour refresh. These rules are not implemented yet; the funding target, taper parameters and hLP claim terms remain unselected.

## Assumptions and units

Funding cases hold protected borrowing/leverage principal at 100,000 USDC, seed the fund with 1,000 USDC, and supply 100,000 USDC of eligible fee base per 30-day month. At 0.2% that yields 200 USDC/month before allocation. These are scenario inputs, not estimates of Dusk activity or universal token-denominated constants. Every fee base unit is assumed collectible without creating a shortfall; real solvent-surplus caps can reduce collection. No extra charge is taken on an insolvent full liquidation.

Each loss case starts independently with a fully funded target, assumes no concurrent fee income and uses the stated shortfall after collateral recovery/rewards. The study does not derive these losses from collateral prices or count debt repayment as fee income. Losses are deterministic fixtures, not probabilities. Funding and loss tables must not be combined as if the seeded fund were already fully funded.

The budget uses raw atoms in the covered debt mint, avoiding a cross-token dollar oracle. No token transfer fee is modeled; real draw accounting must distinguish gross vault debit from spendable debt credit. The proposed 75% targets net loss coverage; the shared fund budget limits gross debit, so transfer fees can reduce achievable coverage. No interest is covered in the borrowing/leverage shortfall examples. Existing hLP terminal funding claims remain a separate claim type; this study does not remove or redefine them.

## Funding results

All fund allocations use the pre-credit balance, clamp to the target, and preserve `seed + collected fees = insurance balance + LP allocation`. The 30-day fee flow is credited daily. A linear taper approaches the target asymptotically, so report time to 95%, not a promise of reaching 100% exactly.

### Target size at 0.2% levy and 75% taper start

| Target / principal | Target | Insurance after 12 months | LP allocation after 12 months | Months to 95% funded |
| --- | ---: | ---: | ---: | ---: |
| 2% | 2,000.00 | 1,989.10 | 1,410.90 | 6.5 |
| 5% | 5,000.00 | 3,400.00 | 0.00 | 23.8 |
| 10% | 10,000.00 | 3,400.00 | 0.00 | 52.6 |

### Replenishment tradeoff at a 5% target

| Levy on eligible base | Monthly designated fee income | Taper starts at | Months to 95% funded | Insurance after 12 months | LP allocation after 12 months |
| --- | ---: | --- | ---: | ---: | ---: |
| 0.1% | 100.00 | 75% | 47.6 | 2,200.00 | 0.00 |
| 0.2% | 200.00 | 50% | 36.3 | 3,256.65 | 143.35 |
| 0.2% | 200.00 | 75% | 23.8 | 3,400.00 | 0.00 |
| 0.2% | 200.00 | 90% | 19.2 | 3,400.00 | 0.00 |
| 0.5% | 500.00 | 75% | 9.5 | 4,908.77 | 2,091.23 |

Starting the taper at 50% gives LPs income sooner but lengthens the underfunded period. Increasing the levy speeds funding by charging more on eligible liquidations; allocating existing fee revenue instead reduces another recipient's yield. These are real transfers, not new yield creation.

### Proposed allocation curve at a 5,000 target

| Insurance balance | Share to insurance | Share to LPs |
| --- | ---: | ---: |
| 0 | 100% | 0% |
| 2,500 | 100% | 0% |
| 3,750 | 100% | 0% |
| 4,375 | 50% | 50% |
| 5,000 | 0% | 100% |

## Spending results

`current_20_event_50_day` mirrors the current native budget arithmetic: 20% of current balance per event and 50% of opening balance plus credited income in the draw window. The alternatives remove the percentage-per-event limit and retain 50% or allow 100% of that shared basis. Day boundaries below mean a complete accounting window has elapsed; they are not UTC-midnight resets.

### Same 2,500 shortfall, different fund targets

| Fund at target | Current: covered / uncovered | Shared 50%: covered / uncovered | Shared 100%: covered / uncovered |
| ---: | --- | --- | --- |
| 2,000.00 | 400.00 / 2,100.00 | 1,000.00 / 1,500.00 | 2,000.00 / 500.00 |
| 5,000.00 | 1,000.00 / 1,500.00 | 2,500.00 / 0.00 | 2,500.00 / 0.00 |
| 10,000.00 | 2,000.00 / 500.00 | 2,500.00 / 0.00 | 2,500.00 / 0.00 |

### A 5,000 fund: what the spending policy preserves or spends

Each cell is **covered / uncovered / fund remaining**. Uncovered borrowing and leverage principal must complete the selected write-off waterfall; it does not remain as a debt on a fully settled leg. hLP claims follow their own terminal loss accounting. No unpaid interest becomes new fee yield.

| Loss sequence | Current 20% event / 50% window | Shared 50% | Shared 100% |
| --- | --- | --- | --- |
| single_2500 | 1,000.00 / 1,500.00 / 4,000.00 | 2,500.00 / 0.00 / 2,500.00 | 2,500.00 / 0.00 / 2,500.00 |
| split_5x500 | 2,500.00 / 0.00 / 2,500.00 | 2,500.00 / 0.00 / 2,500.00 | 2,500.00 / 0.00 / 2,500.00 |
| cluster_5x1000 | 2,500.00 / 2,500.00 / 2,500.00 | 2,500.00 / 2,500.00 / 2,500.00 | 5,000.00 / 0.00 / 0.00 |
| two_days_2500_each | 1,800.00 / 3,200.00 / 3,200.00 | 3,750.00 / 1,250.00 / 1,250.00 | 5,000.00 / 0.00 / 0.00 |
| severe_20000 | 1,000.00 / 19,000.00 / 4,000.00 | 2,500.00 / 17,500.00 / 2,500.00 | 5,000.00 / 15,000.00 / 0.00 |
| borrow_leverage_hlp_2000_2000_1000 | 2,440.00 / 2,560.00 / 2,560.00 | 2,500.00 / 2,500.00 / 2,500.00 | 5,000.00 / 0.00 / 0.00 |

The single 2,500 loss versus five 500 losses demonstrates that the existing per-event percentage makes coverage sensitive to position splitting. The shared-budget alternatives are invariant to those same-window splits, given identical eligible shortfalls, no incoming funds and unchanged parameters. A daily limit still creates ordering competition for a finite shared fund; this is not a proof against fabricated or manipulated losses.

### User candidate: 75% of loss, capped by a shared 50% fund budget

For the no-transfer-fee examples: `payment = min(0.75 * eligible_loss, available_fund, max(0, 0.50 * (window_opening_fund + credited_inflows) - already_drawn))`. All operations sharing this debt-token fund consume the same window budget. This is not 50% of the remaining balance allowed afresh on each liquidation. No additional 20%-of-fund per-event cap is applied in this candidate.

Each row below starts with a fresh 5,000 fund and unused 2,500 budget. The rows are independent, not successive draws.

| Principal shortfall | Insurance payment | Principal written off | Fund remaining |
| ---: | ---: | ---: | ---: |
| 1,000 | 750.00 | 250.00 | 4,250.00 |
| 2,500 | 1,875.00 | 625.00 | 3,125.00 |
| 5,000 | 2,500.00 | 2,500.00 | 2,500.00 |

Repeated 1,000 principal losses in one window receive 750, 750, 750, 250, then 0. The 2,500 aggregate cap binds on the fourth loss; 2,500 remains in the vault. In the next complete accounting window, with no credits, the new budget is 50% of that remaining 2,500 = 1,250. The preserved half is not permanently untouchable.

| Loss sequence | Candidate covered | Uncovered | Fund remaining |
| --- | ---: | ---: | ---: |
| single_2500 | 1,875.00 | 625.00 | 3,125.00 |
| split_5x500 | 1,875.00 | 625.00 | 3,125.00 |
| cluster_5x1000 | 2,500.00 | 2,500.00 | 2,500.00 |
| two_days_2500_each | 3,437.50 | 1,562.50 | 1,562.50 |
| severe_20000 | 2,500.00 | 17,500.00 | 2,500.00 |
| borrow_leverage_hlp_2000_2000_1000 | 2,500.00 | 2,500.00 | 2,500.00 |

For one 2,500 principal loss or five 500 losses, candidate coverage is 1,875 either way. Raw-atom rounding can make split coverage slightly smaller, never larger in the tested same-window fixed-parameter examples. A loss must not be claimed again on the uninsured remainder: full settlement covers once, writes off the rest and clears that debt leg.

The mixed borrowing/leverage/hLP fixture applies the percentage uniformly only as a budget sensitivity. It does not select a 75% payout rule for hLP funding-interest claims. Their entitlement must be explicit while sharing the aggregate draw budget; existing protection must not silently change through a generic insurance helper.

## hLP and target stability

Source inspection confirms that hLP terminal settlement also calls the same insurance draw-capacity function. Its present shortfall is bounded by funding interest, not the borrowing/leverage principal-only policy. Therefore a target based only on external-loan principal omits an existing insurance claimant. A candidate definition is `target = max(minimum_seed, q * covered_principal + hLP_loss_allowance)`, with the allowance estimated from hLP stress and no double counting. The funding table holds that extra allowance at zero solely to isolate the selected principal-target sensitivity.

For 100,000 protected principal and q=5%, hLP allowances of 0 / 1,000 / 5,000 produce targets of 5,000 / 6,000 / 10,000. A current 5,000 balance is respectively 100% / 83.33% / 50% funded. Those allowances are examples, not loss estimates.

The implementation must reserve pending insurance obligations and avoid an instantaneous manipulable denominator. Capture pre-liquidation exposure for the fee quote; bind its amount/allocation through atomic settlement, and use a justified observation/target adjustment rule across transactions. A same-slot borrow/repay or LP withdrawal must not cheaply redirect funding or release the existing fund. Target decreases affect future fee allocation only. Shared hLP/borrow/leverage ordering and draw windows need native instruction tests.

## Computational cost and model limits

The proposed funding split needs bounded integer arithmetic: compare to knee and target, multiply by remaining target gap, divide by taper width, clamp. It needs no transcendental calculation or scan of every position. Aggregate insured exposure, reservations and target observation still require accounting state and measured compute tests; cheap math is not zero-cost implementation.

The pre-credit allocation rule is sensitive to fee batching inside the taper. Production rounding, sub-atom carry and split/merge incentives need an explicit policy; this study uses equal daily credits. No price manipulation, future liquidity, fee elasticity, keeper behavior, interest accrual, token CPIs or revised time-based liquidation outcomes are simulated. The 5% target and 75% knee are proposals for integrated testing, not sufficiency guarantees.

## Validation and reproduction

99 deterministic cases. Assertions cover cash conservation, bounded/monotone allocation, native-budget arithmetic anchors, new credits, window reset, same-window splitting under shared caps, and increasing coverage with relaxed caps, 75% loss sharing, aggregate cap exhaustion and sub-atom splitting. This Python study is not an executable native-code parity test.

```sh
PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/insurance_policy_analysis.py
```

Raw amounts are USDC micro-units: [funding CSV](insurance-funding-sensitivity.csv), [draw CSV](insurance-draw-sensitivity.csv). No build products are required.

Source anchors: `programs/dusk/src/transitions/ledger.rs` (`Insurance::draw_capacity`), `programs/dusk/src/constants.rs` (20% / 50% ceilings), `programs/dusk/src/transitions/liquidity/hlp/engine.rs` (shared terminal claims).
