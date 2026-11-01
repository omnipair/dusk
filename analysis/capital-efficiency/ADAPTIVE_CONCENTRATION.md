# Adaptive concentration: costs determine viability

## Executive Summary
- **Fixed 10× remains the profit benchmark in the expanded test.** It earns 4,600 USDT per six-hour evaluation run on average, versus 1,819 for the selected profitability controller, 3,635 for the volume controller and 3,319 for the volatility controller. All include LP-owned base, volatility and outward fees, plus trading losses.
- **Changing concentration can create its own arbitrage loss.** The profitability controller collects more fees but spends 2,676 USDT per run on immediate arbitrage after changing the curve. Reserves are preserved; the marginal price changes.
- **A cost gate makes adaptation much more credible, without establishing a profit advantage.** The exploratory cost-gated controller earns 4,509 USDT, 2.0% below fixed 10×, and has 26.6% lower mean trading drawdown. It needs fresh-date validation before selecting it as a risk control.
- **Current Dusk cannot run this minute-scale policy through its existing concentration update.** Governance has a seven-day delay and protected-depth funding constraints. Prices are historical, customer demand is modeled, and the fast controller is a research design.

| Policy | Mean net P&L | Mean immediate switch loss | Mean max drawdown |
|---|---:|---:|---:|
| Omnipair v1 | 3,051.47 | 0.00 | 134.46 |
| Fixed 3× | 3,359.82 | 0.00 | 14.56 |
| Fixed 5× | 3,985.54 | 0.00 | 19.96 |
| Fixed 10× | 4,599.64 | 0.00 | 33.47 |
| Profitability signal | 1,818.54 | 2,676.20 | 2,259.53 |
| Volume signal | 3,634.53 | 89.06 | 53.82 |
| Volatility signal | 3,318.65 | 824.53 | 705.20 |
| Cost-gated (exploratory) | 4,509.49 | 17.43 | 24.56 |

All values in USDT per six-hour run; 1m initial capital, 96 paired evaluation cases. Cost-gated result is exploratory.

## Automatic switching did not beat fixed 10× on trading profit
**The new comparison includes 1,440 primary replays and 96 exploratory gated replays.** In the later-date evaluation, fixed 10× averages 4,599.64 USDT of net trading P&L per six-hour run. The training-selected profitability controller earns 1,818.54, a 60.5% reduction. Volume-only and volatility-only controls also trail fixed 10× on average.

The chart compares the same 96 price/demand/seed cases per policy at 1m USDT starting capital. The cost-gated result was evaluated after inspecting the primary failure and is explicitly exploratory. These averages give each designed demand scenario equal weight; they do not estimate the frequency of those conditions in a real pool.

**The objective is trading only:** LP-owned fees plus signed execution edge, with between-trade inventory price gains removed. Both dynamic fee components remain enabled for Dusk. Immediate switch losses are already included; subtracting them or LVR again would double-count.

## Decisions use past data, with reserves carried through every change
**Training uses January 8–9 and evaluation uses January 15 and 19, with new demand seeds.** Each day supplies four separate continuous six-hour blocks at one-second resolution. Every block starts balanced, centered and cold; there is no reset inside the six hours. Twelve fixed/controller candidates train on two seeds, then one controller from each signal family and the best fixed benchmark are frozen before the main evaluation on three seeds. Some hours were inspected in the preceding analysis, so this is not a fully blind validation set.

All adaptive policies start at 5×, observe a ten-minute warmup, decide once per minute and wait at least five minutes between changes. They can move one step among 3×, 5× and 10×. Decisions happen before the next reference tick and arriving orders. Fixed pools run throughout the same warmup; all six hours count toward profit.

The profitability signal uses completed 30-second markouts for all swaps over the preceding five-minute maturity window. It divides fees plus favorable execution markouts by adverse markout losses, with a 1-USDT denominator floor. The selected thresholds are 1.05 and 1.5: choose 3× below the lower threshold, 10× above the upper one and 5× in between. The controller sees no customer/arbitrage labels or rejected intentions. Volume and volatility controls use only past executed turnover and external returns.

Demand is modeled as either steady strong, steady weak, steady cheap execution, or randomly changing half-hour regimes. Prior January 1 CEX clips supply only a size proxy; directions, arrivals and cost tolerance remain explicit assumptions. Every paired policy receives identical offered orders and budgets, with fills responding to its own quotes.

## Extra fee income did not cover the losses created by reconfiguration
**The profitability controller earns 8,131 USDT in LP fees, versus 6,305 at fixed 10×, yet finishes much worse.** It changes concentration 30.2 times per six-hour run on average. Immediate arbitrage following those changes costs 2,676 USDT, compared with a 2,781-USDT total profit gap. The switch-loss comparison identifies an accounting component; it is not a claim that simply adding it back reproduces a no-switch counterfactual.

Changing the curve leaves both token balances intact, but it changes their implied marginal price. Arbitrageurs can trade against that new price. The volatility accumulator is preserved, all fees apply, and greedy profitable arbitrage is settled before the next external-price tick. There is no free recenter, capital injection or inventory reset.

The profitability controller trails fixed 10× on both evaluation days. The January 19 day is worse, but the result is not confined to that day. Delayed profitability can also respond to trades induced by the controller itself; a fee-rich signal does not establish profitable incremental demand.

## Demand conditions still matter for the fixed concentration choice
**Strong fee-tolerant demand favors fixed 10×, while steady weak demand favors fixed 3×.** In the cheap-execution scenario, v1 has the highest mean of the tested fixed curves. The changing-demand scenario also favors fixed 10× in this sample, despite the mix of regimes.

The table reports mean net trading P&L in USDT per six-hour run, with 24 paired cases per demand mode. Weak demand is 100k expected offered volume per hour and a 20-bps median cost budget. Strong demand is 1m/hour and 100 bps; cheap execution is 1m/hour and 50 bps. The changing mode draws those three regimes plus 100k/hour with 100 bps every half hour. These are offered-volume assumptions, not realized captured volume.

## A switch-cost gate preserves most profit and reduces sample drawdowns
**The gated controller averages 4,509.49 USDT, versus 4,599.64 at fixed 10×.** It permits a change only when the estimated immediate arbitrage loss is at most 25% of positive net markout P&L over the trailing window. It uses the predeclared 1.1/2.0 coverage thresholds. Average immediate switch loss falls to 17.43 USDT, although it still executes 20.9 changes per run.

Mean maximum trading drawdown is 24.56 USDT versus 33.47 for fixed 10×, a 26.6% reduction. Its worst six-hour trading outcome is -108.25 versus -198.61. It beats fixed 10× in 38 of 96 paired cases and remains 2.0% behind on average.

**This is a possible risk/profit tradeoff, not demonstrated extra profitability.** The candidate was specified before training but evaluated on these later dates after the primary result was inspected. Its gate and coverage thresholds differ from the selected ungated policy, so their difference is not a clean one-factor estimate of the gate's effect. Confirmation requires untouched dates and a preselected risk objective.

## Additional transaction costs only widen the profit gap
**All primary numbers already include the endogenous cost of repricing.** The chart below deducts 0, 0.1, 1 and 10 USDT per executed concentration change for additional transaction or operational overhead. It keeps the simulated decisions and reserves fixed and pays that overhead from the external fee wallet.

No tested controller has positive average profit headroom versus fixed 10× at zero added overhead in this evaluation. Therefore none has a positive average break-even extra cost per switch for beating that benchmark. This says nothing about a different objective that explicitly pays for drawdown reduction.

## The current concentration update cannot execute these fast changes
**The existing path is a governance parameter update with a seven-day timelock after support approval.** A minute-scale controller would need an explicit design for delegated or automatic authority; this analysis changes no protocol code.

Reconfiguration also requires a protected-depth budget. A depth decrease must be covered at 125%, plus a 1-bp guard on current depth, and fit a hard cap of 1% of depth. At centered unchanged reserves, moving from 10× to 5× requires 62.51% of current depth under this formula; moving from 10× to 3× requires 87.51%. Both exceed that hard cap. Distributing fees to LPs does not automatically create the in-pool protected-depth budget.

The economic replays deliberately relax these admission rules so the switching hypothesis can be tested. Reserve conservation and repricing loss are modeled, but those results are not executable current-program returns. Changing how depth protection treats a different curve shape is a separate design question; bypassing the guard is not established as safe.

## The arithmetic is feasible off chain; protocol integration remains unmeasured
**The local research engine completed 1,440 primary replays in about 134 seconds.** A small warm Python benchmark rebuilt a reserve-preserving curve in roughly 0.066 ms per call. These are local CPU observations, not Solana compute-unit measurements or a production latency guarantee.

The policy needs an external price reference and delayed swap markouts. Its work scales with observed swaps; the queues store pending 30-second markouts and recent five-minute observations. Pool fee revenue or its internal volatility accumulator alone does not identify toxic flow. An implementation must address reference quality, manipulation and the cost of keeping these observations.

Current Rust curve reconstruction already uses a bounded branch selection and quadratic-root approach. The new economic risk lies in deciding when a change is worth its repricing loss, along with authorization and protected-depth accounting. No automatic controller, integer parity or compute-unit test was implemented here.

## Keep fixed 10× as the profit benchmark and test cost-aware adaptation for risk
**The evidence supports a fixed benchmark and a narrowly defined next experiment.** Fixed 10× leads this test grid on mean trading profit. Fixed 3× remains relevant for weak demand, and cheap execution can favor v1. A lower concentration should be selected with the demand regime and execution objective in view.

For adaptive concentration, take only the cost-gated candidate forward to fresh dates, with a risk objective declared before testing—for example, acceptable trading drawdown at a specified profit concession. Compare it with fixed 3×, 5× and 10× again. Require causal signals, an estimate of repricing cost, and enough evidence that the incremental benefit can pay that cost.

The current sample cannot establish real-world demand capture, market-frequency weights, robustness to strategic manipulation, or a production policy. It has four historical days, two later-date evaluation days and modeled customer demand. Protected-depth semantics and controller authority remain separate unresolved design requirements.

## Reproduce

```sh
python3 analysis/capital-efficiency/adaptive_replay.py --workers 4
python3 analysis/capital-efficiency/adaptive_diagnostics.py
python3 analysis/capital-efficiency/adaptive_report.py
```

Full report: [report.html](report.html). Plan and selection: `adaptive_plan.json`, `adaptive_selection.json`. Raw rows: `adaptive_replays.csv`, `adaptive_gated_replays.csv`. Checks: `adaptive_validation.json`, `adaptive_extra_validation.json`, `adaptive_verification.sql`. Publisher URLs and checksums: `market-data/manifest.json`. No program code changed.
