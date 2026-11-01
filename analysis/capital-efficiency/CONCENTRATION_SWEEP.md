# Fixed-fee concentration sweep

## Executive Summary
- **Retain 3×, 5× and 10× as contrasting candidates; no single optimum is established.** Twelve Dusk concentrations were tested with identical fee parameters and widths. Trading outcomes depend on order size, routing tolerance and the price path.
- **Execution gains diminish as concentration increases.** A warm $10k buy matches v1 using $737,991 at 5× and $659,426 at 10×. The extra capital saving is 7.9 percentage points, while same-capital warm +5% jump loss rises from $365.97 to $501.42.
- **More fee-paying demand can justify more concentration; volatility alone does not.** Synthetic one-hour paths count customer and arbitrage trading P&L after LP fees. Large-order choppy flow rewards high concentration in some cases; quiet-to-jump paths can leave the same configuration much worse off.
- **Some execution targets cannot be reached at any concentration.** With a warm signal, the minimum base-plus-volatility all-in cost is about 80.24 bps, above v1's 50.09-bps $1k quote. These are modeled conditional results, not expected market returns.

## Fix fee parameters and vary concentration
**Only amplification changes across Dusk configurations:** 1.25×, 1.5×, 2×, 3×, 4×, 5×, 7.5×, 10×, 15×, 20×, 30× and 50×. Every configuration keeps the 100-bps core, 400-bps additional shoulder and a positive full-range tail. Fixed fees: 30-bps base, outward coefficient 10, volatility coefficient 1, 200-bps cap per surcharge, and 60-second volatility half-life. Fixing parameters does not fix realized fees: each fee still responds to its trade and prior observed pool-price movements.

Equal-capital tests start with $1m ordinary reserves. Equal-execution tests resize each independent pool to match v1's order output, then recompute shock losses on that actual capital. All surcharges are distributed to LPs; the protocol takes 20% of the base fee. Widths, center and concentration remain fixed throughout each path.

**Trading-only P&L = LP fees plus signed execution edge against the external price at each trade.** Both customer and arbitrage trades count. Inventory gains caused by external-price moves between trades are removed and reconciled using an independent endpoint inventory ledger. These are event losses and synthetic one-hour results, without an APR interpretation.

## Capital savings flatten as concentration rises
**5× captures much of the execution benefit under these fees.** The warm $10k order needs $870,277 at 3×, $737,991 at 5×, $659,426 at 10× and $602,289 at 50×, versus $1m for v1. The target all-in cost is 230.09 bps.

Quiet and warm are separate conditional accumulator states, 0 and 0.005. Quiet still pays the outward surcharge. These labels do not specify the variance of the next price move. The chart shows 1.25–20× to keep the lower part of the sweep readable; complete CSVs include 30× and 50×.

## Less capital lowers dollar exposure, but loss per capital still rises
**At matched execution, 5× and 10× have different jump exposure.** The warm +5% same-slot greedy-arbitrage loss is $270.09 at 5× and $330.65 at 10×, versus $275.90 for v1. Those losses are 3.66, 5.01 and 2.76 bps of each pool's own capital. A lower dollar loss on a smaller pool does not establish a better return per dollar.

Both surcharges are included and affect the arbitrageur's chosen trade size. Each burst allows repeated immediate-profit-maximizing trades, updating the volatility signal. It is not globally optimized strategic splitting. The chart recomputes losses on capital matched under the same initial fee state; v1's dollar benchmark is $275.90. Later decay and trading appear in the one-hour paths below.

## Trading profits depend on the path and on which orders fill
**The paths include all trading profits and losses.** Each run lasts one synthetic hour, starts cold and centered with $1m, samples external prices every 30 seconds, and offers the same 60 seeded buy/sell intentions. An order fills only when all-in cost is at most 150 bps versus the current external price. Arbitrage runs before and after accepted orders; both fees follow the pool's own evolving state.

The table uses $2k per intended order ($120k offered per hour). At a flat external price, v1 earns $472.88 and Dusk 5× earns $588.20. With a quiet-to-5% jump, v1 earns $226.57 while Dusk 5× loses $137.66. Both surcharges are active, but concentration does not win on every path.

The slow trend compounds evenly to +5%; the jump reaches +5% halfway through and stays there. The choppy path completes ten smooth cycles around the center with roughly ±1% price movement. Fills are allowed to differ between pools; these are common opportunities, not identical forced executions. Runs without customer orders isolate adverse selection.

## High concentration can pay when it captures enough large orders
**Large-order access changes the ranking.** On the choppy path with $10k intended orders, 5× fills 21 of 60 and earns $1683.09; 50× fills 48 and earns $3864.27. V1 fills none at the 150-bps limit and loses $177.15 to arbitrage. This is an explicit demand assumption, not evidence that actual customers supply that volume.

With $2k orders on the same price path, 10× earns $494.23 while 50× earns $198.12. The chart changes order size while keeping fee parameters and the external path fixed. Each point includes fees and signed trading edge.

## Routing assumptions change the winner
**Exact optima are fragile under this demand model.** A sensitivity grid tests v1 and Dusk at 2×, 3×, 4×, 5×, 10×, 20× and 50×. It uses routing limits of 100 and 300 bps with the original order sequence, plus two additional order-direction seeds at 150 bps: 256 additional runs, with all fee parameters unchanged.

For $10k orders on the choppy path, 50× leads the original 150-bps run and both alternate sequences. Tightening the limit to 100 bps makes 4× lead the sensitivity grid; loosening it to 300 bps makes 10× lead. Flat-price, smaller-order rankings also vary. Hard routing thresholds produce discontinuous fills, so these are scenario winners, not smoothly fitted optimal parameters.

**Retain 3×, 5× and 10× for the next calibrated comparison:** a lower-exposure candidate, an intermediate candidate with substantial execution gains, and a candidate testing whether extra captured demand pays for larger jump losses. Keep 20–50× as stress cases. This is an experimental shortlist, not a deployment recommendation.

## Concentration cannot remove the minimum fee toll
**A warm $1k order cannot match v1 at any tested concentration, or even at infinite depth in this model.** At an accumulator of 0.005, base plus volatility takes approximately 79.60 bps of gross input. Expressed as all-in cost per unit received, that is 80.24 bps before outward surcharge or price impact. V1's $1k quote costs 50.09 bps including impact. The cool and hot states also fail this target.

These cases are recorded as `unattainable_fee_floor`, with null capital rather than a fabricated large-capital solution. The fees stay unchanged as requested. A later small-order optimization would need to revisit fee settings or the execution target.

## Calibrate demand before testing an automatic concentration rule
**This sweep supports conditional concentration choices, not a rule that higher volatility should automatically increase concentration.** Next compare the shortlisted fixed configurations using realistic order intentions and external-price updates, then test an explicit controller against those fixed baselines. Changing amplification can move the quoted price and transfer value; that economic cost belongs in the controller comparison.

The model uses real-valued input-denominated fees; no integer program parity or compute-unit claim is made. Both pools distribute fees without compounding. No hLP, debt, retained fees or center movement is included. Greedy arbitrage observes these synthetic prices every 30 seconds, which is not a Solana block-time model. Price movements within those intervals are unobserved. Fees paid in base are valued as if immediately converted at the external mark.

The scenarios have no estimated occurrence probabilities. Three order sequences and three cost limits are sensitivity tests, not confidence intervals. Trading-only P&L excludes directional inventory gains and does not subtract LVR again. The main unresolved quantity is actual profitable customer flow attracted by each concentration relative to adverse selection.

## Exact lookup: warm $10k execution and warm +5% jump

| Configuration | Required capital | Capital saved | Loss on resized pool | Loss / own capital |
|---|---:|---:|---:|---:|
| v1 | $1,000,000 | 0.0% | $275.90 | 2.76 bps |
| 2× | $1,049,053 | -4.9% | $272.77 | 2.60 bps |
| 3× | $870,277 | 13.0% | $270.71 | 3.11 bps |
| 5× | $737,991 | 26.2% | $270.09 | 3.66 bps |
| 10× | $659,426 | 34.1% | $330.65 | 5.01 bps |
| 20× | $623,146 | 37.7% | $619.89 | 9.95 bps |
| 50× | $602,289 | 39.8% | $1490.55 | 24.75 bps |

## Reproduce

```sh
python3 analysis/capital-efficiency/concentration_sweep.py
python3 analysis/capital-efficiency/concentration_report.py
```

The first command calculates scenarios and validation. The second preserves and extends the complete report, recomputes chart metrics in SQLite, and adds executed notebook cells. Raw outputs: `concentration_results.json` and `concentration_*.csv`. Source hashes and checks: `concentration_validation.json`.
