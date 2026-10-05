# Preliminary native leverage calibration

**Latest experiment (2026-10-05):** [solvent repayment-cushion comparison](REPAYMENT_CUSHION_RESULTS.md)
models an earlier fully funded internal sale alongside the separate loss-taking
fallback, using the selected discount candidate and a provisional curve meeting
the 6x margin target. It compares competing partial/full ordering and price gaps.
This is economic modeling, not implemented runtime policy or selected defaults.

Earlier [stored-depth margin comparisons](STORED_DEPTH_RESULTS.md) and
[fixed-payment flash purchase economics](FLASH_PURCHASE_RESULTS.md) preserve
the previous margin/discount candidates; their numeric settings are not defaults.
The sections below preserve the older experiments.

**Historical results:** the [2026-10-05 decisions](../LIQUIDATION_DECISIONS.md)
supersede several policies modeled here. These experiments have not been rerun
for fixed-payment flash purchases, reference-only eligibility, timer-free
emergencies and the selected composability rules. Keep their limitations and
provenance when comparing new candidates.

Date: 2026-10-02. Base: PR #45 at `5b123acf30574473d4a9346fbd1195d6d6cf1733`, with the uncommitted calibration modules in this checkout.

These results reject some illustrative settings. They do not select final risk parameters or establish production safety.

## Revised emergency policy comparison (2026-10-02)

Read the [current policy](../EMERGENCY_LIQUIDATION_POLICY.md) and [generated results](EMERGENCY_MARGIN_RESULTS.md). This adds 864 entry/settlement comparisons and 249 sensitivity cases in an independent CPMM economic model. Twelve native full-liquidation anchors cover both debt directions, prices 0.0004/1/2500, and decimal pairs 6/9 and 9/6; all match the zero-fee quote and write-off reserve identities. Eleven model tests exercise clocks, insurance, principal/interest recovery, splitting at fixed depth and unprofitable cleanup. The model is not the new runtime implementation.

### What this comparison establishes

1. **No permanent historical-price veto:** in the fast-decline fixture with $7,500 debt across ten positions, a permanent protected floor leaves all ten eligible positions unsettled at the horizon. Terminal internal settlement clears them but realizes about $2,165 principal loss after $500 insurance. The apparent zero realized loss with the permanent floor conceals unresolved debt.
2. **Normalizing gentle MM does not cure late intervention:** the historical fixed bands and depth-normalized 7/8/10% bands both lose about 28.87% of that admitted debt in this fast path. Higher initial collateral or a 2:1 IM/MM relationship does not independently fix a late trigger. The faster path is a 3% marginal-price decline per ten-second sample, with a 60-second EMA; it is a stress scenario, not a forecast.
3. **Earlier/higher MM and stack assignment have an entry cost:** at $15k requested spending split into ten entries, the earlier position-size candidate admits ten with the buffer rule; stack-at-entry admits eight. With double-MM entry requirements the counts are nine and four. Do not attribute lower loss solely to a better liquidation mechanism when the candidate admits less debt.
4. **Internal-only terminal execution can discard better outside recovery:** an additional, unapproved terminal flash candidate binds delivery to the program's internal recovery baseline. With 5x outside depth, the same fast case loses about $1,699 under floor-only delivery or $1,553 if all route output reaches settlement, versus $2,165 internally. The first comparison still allows the keeper to retain external output above its obligation. Neither is a best-execution guarantee or proof that the internal baseline is manipulation-safe.
5. **Keeper cost and insurance segmentation matter:** percentage rewards leave small modeled positions unfilled at sufficiently high fixed cost. More positions also access the per-event insurance cap differently, even with the same daily cap. Compare pre-insurance shortfall as well as principal write-off; low realized loss does not imply less total system loss.

### Parameter status and limitations

No numeric schedule is selected. The gentle schedules fail meaningful stressed loss tests; the earlier schedule and stack alternative reduce some losses but add leverage/UX costs. The approved product direction still favors individual-size MM and aggregate-exposure IM; stack assignment and the external terminal rule remain alternatives requiring a decision. We need an accepted scenario loss budget before describing a schedule as calibrated for release.

The simulator uses plain constant-product execution with an input fee, no hLP/public borrowing, no token transfer fees, no accrued interest in the sweep, and no optimized split routing. Its independent waterfall tests include interest. The native anchors have zero AMM fee and no insurance, so they do not validate all of those proposed rules. The sweep uses 5% partial-sale steps and one opportunity per position per observation; both introduce execution delay/size granularity. Dusk receives specified adverse spot flows after entry and has no restorative arbitrage; outside fixtures are repriced between samples and finite within a sample. LP withdrawal is a proportional-depth stress, not a native instruction or assertion that every withdrawal passes runtime guards. Concentrated curves and manipulation of terminal snapshots remain unresolved; the original PoC is not run.

The maximum distress age is not a promise of automatic cleanup: critical reference health can trigger earlier, missed observations reset confirmation, keepers must submit, and physical cash/transfer constraints still apply. The normal protected floor and progressive reward inputs are also candidates. Rejecting a route at the floor is not evidence that all outside liquidity disappeared.

### Reproduce the new comparison

```sh
cargo test -p dusk margin_calibration_native_terminal_anchors -- --nocapture > /private/tmp/dusk-terminal-anchors.log 2>&1
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s docs/calibration -p test_emergency_model.py -v
PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/emergency_model.py --native-log /private/tmp/dusk-terminal-anchors.log
```

The command verifies the successful native log and reproduces `terminal-native-anchors.csv`, `emergency-margin-comparison.csv`, `emergency-policy-sensitivity.csv` and `EMERGENCY_MARGIN_RESULTS.md`. Native-anchor identities are checked in integer token atoms; the exploratory path simulation uses floating-point token units, not an implementation of on-chain rounding.

The sections below preserve the earlier native experiments and their scope. They are not replaced by claims of native execution of the new policy.

## Entry scenarios

The harness executes Dusk's native prepared swap and leverage debt/cash accounting, then evaluates the existing and candidate admission rules. It does not infer success from the mutated state of a failed on-chain open.

The sweep includes $25k/$100k/$1m initially balanced markets, curve amplification 1 and 4, both debt directions, five entry sizes, and four wallet-equity fractions. Tokens have three decimals. The AMM fee is 0.3%; token transfer fees and external venues are not yet modeled. The candidate denominator is twice the smaller reference-valued cash side after entry. The candidate maintenance bands are fixed at $10k/$30k for this experiment, not rescaled by market size.

Of 240 attempted cases, 212 produced native entry/exit results; 28 were rejected by native execution. The illustrative replacement admitted 116 of the 212, while the existing complete admission rules admitted 36. This measures admission differences, not liquidation safety.

Examples for $100k initial liquidity, amplification 1, quote debt:

| Entry spending | Wallet share | Reference health after entry | Executable health after entry | Candidate result |
| --- | --- | --- | --- | --- |
| $1,000 | 20% | 18.15% | 19.52% | Pass |
| $10,000 | 20% | 3.75% | 19.55% | Reject |
| $10,000 | 33.34% | 19.80% | 32.97% | Pass |
| $25,000 | 33.34% | 0% (equity metric saturates) | 33.00% | Reject |
| $25,000 | 50% | 24.84% | 49.74% | Pass |

The entry size is the amount spent, not the post-entry reference-valued collateral. Entry changes the curve; the latter can be substantially smaller. Both health checks matter.

## Partial recovery scenarios

These are independently seeded liquidation-state fixtures, not positions claimed to have passed the current admission rules. The $100k label denotes starting curve depth. Seeded isolated debt reduces physical reserve cash while preserving the native reserve identity; cash liquidity is therefore below that label. The test searches sale sizes in 1% increments of position collateral. An unsuccessful search does not prove an exact optimum; the explicit $10k/7% regression additionally checks the full-unwind shortfall and insufficient reference recovery on that grid.

The model uses zero keeper reward, no token transfer fees, and native AMM accounting. This is an optimistic recovery test. Remaining reference and executable health must both reach the tested maintenance rate plus 2 percentage points. Full debt repayment with collateral remaining also counts as feasible. The reported first feasible sale is grid resolution, not an exact minimum.

For $10k reference collateral against $100k plain curve depth, a full sale yields approximately $8,312.49 after AMM fees:

| Starting equity / tested maintenance | Debt | Recovery target | First feasible fraction sold |
| --- | --- | --- | --- |
| 7% | $9,300 | 9% | None |
| 10% | $9,000 | 12% | None |
| 15% | $8,500 | 17% | None |
| 20% | $8,000 | 22% | 81% |
| 30% | $7,000 | 32% | 47% |

These are sensitivity cases using a constant maintenance rate per row; they are not proposed progressive bands. At 7%, the best reference health found is 7.71%, below the 9% recovery target. The debt also exceeds the proceeds of a full unwind. At 15%, reference-only recovery can look healthy while executable recovery still fails.

## Implications and remaining work

- Removing the standalone 2% rule permits useful additional entries, but does not justify accepting the illustrative maintenance schedule.
- Large/thin-market positions require earlier liquidation, reliable external recovery, or explicitly accepted losses. A higher initial deposit alone does not fix a liquidation trigger that waits too long.
- The additional price-path experiment below compares intervention times. Next simulate executed partial/emergency settlements, multiple positions, transfer-fee schedules, LP withdrawals, external liquidity loss, and the selected reward/trigger rules. Calibrate progressive bands using those results.

## Emergency trigger price paths

The additional `margin_calibration_emergency_price_path_report` test opens positions through native debt/swap accounting, then applies actual native spot swaps and risk observations. All openings pass the illustrative candidate's reference and executable initial-margin checks; this is not a claim that they pass the existing standalone 2% limit. The paths use $100k initial balanced curve depth, amplification 1/4, both debt directions, $1k/$5k/$10k entry spending, and 50% wallet equity. Each subsequent sale inputs 1% of the current collateral-side physical reserve. Swaps are separated by either 25 slots (10 modeled seconds) or 150 slots (60 modeled seconds), using Dusk's 400ms target slot duration. All three risk half-lives are explicitly 60 seconds for this experiment. The earlier static fixtures retain their zero-half-life test configuration.

For each of 24 paths, six rows record the first sampled point where **both** reference and executable equity cross 20%, 15%, 7%, 5%, 3%, or 0%. No liquidation is executed between rows. These are counterfactual intervention points along the same unliquidated history, not repeated liquidations of one position. Values can overshoot the threshold between samples. Zero equity saturates insolvency; it does not mean debt exactly equals collateral value.

The illustrative keeper debit is 0.25% of realized output, rounded down, before debt repayment. The fixed rate has been superseded by the user's preference for a bounded progressive reward, including insolvent cleanup; the historical experiment has not been rerun with that new curve. There are no token transfer fees, interest accrual, external venues, LP withdrawals, hLP positions, public borrowing, or other leverage positions in this path experiment. The reported shortfall is the debt unrecovered after that debit, **before** any insurance or socialization; the test does not execute the loss waterfall. The selected settlement requirement is to draw eligible insurance and write off any remaining obligation atomically, clearing the fully liquidated debt leg.

Example: $10,000 entry spending with $5,000 wallet equity and $5,000 debt, plain curve, quote debt. Amounts below are rounded to cents. The initial $100k depth changes along the path.

| Reference threshold tested | Slower-path actual reference equity | Slower-path full AMM output | Slower-path shortfall | Faster-path shortfall |
| --- | --- | --- | --- | --- |
| 20% | 19.55% | $5,220.02 | $0.00 | $515.12 |
| 15% | 14.62% | $4,936.20 | $76.14 | $760.01 |
| 7% | 5.71% | $4,496.12 | $515.12 | $1,139.55 |
| 5% | 3.82% | $4,412.79 | $598.24 | $1,211.39 |
| 3% | 1.90% | $4,330.97 | $679.86 | $1,281.92 |
| 0% | 0.00% | $4,250.62 | $760.01 | $1,351.18 |

This supports avoiding an additional wait for near-zero reference equity once liquidation is justified. It does **not** establish 20% as a safe universal trigger: the faster path already loses money at that threshold. The tests assert that distinction and that later thresholds do not reduce losses on these declining paths. Constant position debt makes these optimistic with respect to interest accrual.

### Subsequent product direction

The original early-fallback proposal based only on no feasible protected Dusk repair was refined after discussion. The candidate now combines reference-confirmed eligibility, no protected internal partial repair, and a critically low executable repayment cushion confirmed across observations or a separately critical reference level. These precise conditions and thresholds remain to be calibrated. No internal quote can prove that every external route was unavailable, and current AMM depth/price affects the result.

An unfillable flash transaction cannot leave a persistent failed-attempt marker: it reverts. A successful public "failed attempt" marker would only prove that someone submitted it, not that external liquidity was unavailable. Neither should be used as proof that a floor exception is justified.

The user prefers progressive incentives that remain available for insolvent liquidation and explicitly accepts competitive keeper racing as a design assumption. The reward curve, cap, and any time supplement remain unselected. A proceeds-funded reward increases the amount to resolve through insurance/write-off when debt is not fully repaid. Full settlement must complete that waterfall in the same transaction, not leave the reported pre-insurance shortfall as unresolved position debt.

Atomic settlement and bounded incentives do not remove execution-price manipulation risk once the ordinary reference floor is waived. Keep audit finding #287652 open as an economic design risk until the selected fallback is explicitly documented and assessed. No supplied exploit PoC was executed.

## Reproduction

Run `cargo test -p dusk margin_calibration -- --nocapture`. The `MARGIN_CALIBRATION`, `RECOVERY_CALIBRATION`, and `EMERGENCY_CALIBRATION` CSV lines correspond to the attached tables. Also run `cargo test -p dusk math::leverage_margin` for integer arithmetic and property checks. Extract a saved combined log with `python3 docs/calibration/extract_reports.py /path/to/test.log`; repeated headers are ignored and each marker writes its own CSV.

Sources: `programs/dusk/src/tests/transitions/leverage_margin_calibration.rs` and the actual Dusk transitions it calls. Outputs: [entry candidates](leverage-entry-candidates.csv), [partial recovery](leverage-partial-recovery.csv), and [emergency price paths](leverage-emergency-paths.csv).
