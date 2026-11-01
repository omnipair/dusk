# Historical-price shortlist: fixed fees

## Executive Summary
- **10× becomes the leading candidate when substantial demand accepts the fee schedule.** With 1 million USDT of expected offered volume per hour and a 100-bps median customer cost budget, 10× has the highest five-seed mean in all five historical-price episodes. It beats 5× in 21 of 25 paired seed runs, so the advantage is not universal.
- **Lower concentration remains useful when flow or fee tolerance is weak.** At 100,000 offered volume and a 20-bps median budget during the −8.08% hour, net trading P&L is -45.94 USDT at 3×, -63.97 at 5× and -111.48 at 10×. These are losses after LP fees.
- **There is no unconditional winner across the demand grid.** Of 40 scenario means, 10× leads 20, 3× leads 9, 5× leads 8 and v1 leads 3. These counts describe the chosen test grid, not market probabilities.
- **Prices are historical; demand remains modeled.** The 1,015 replays use checksum-verified SOL/USDT prices and a prior-day CEX clip-size proxy. Dusk customer capture and fee tolerance are unobserved. No production setting, expected APR or automatic concentration policy is established.

## Historical prices replace the invented paths; demand is explicit
**The comparison now uses five historical SOL/USDT hours.** They represent the 10th, 50th, 90th and 99th percentiles of hourly realized variance, plus the largest absolute hourly move, among 720 hours from January 2–31, 2025. Selection used external-price statistics before any simulated LP results. Each episode starts centered, balanced and cold at its own opening price; this is not a continuous month-long backtest.

All fee controls remain fixed: 30-bps base, outward coefficient 10, volatility coefficient 1, and 200-bps cap per surcharge with a 60-second half-life. Dusk uses 3×, 5× or 10× concentration with 100/400-bps core/shoulder widths. The baseline is v1 with the same base fee and protocol share. Main runs use 1 million USDT capital; both surcharge components accrue to LPs.

The simulation uses each completed second's observed price, then processes modeled customer intentions and arbitrage at that boundary. The chart samples every 30 seconds for readability; the replay itself uses 1-second data. Prices are last-trade proxies from one CEX, not a consolidated efficient mid.

## Use observed clip sizes without calling them Dusk customer demand
**The size proxy comes from 238,579 aggregated trade clips on January 1, before every test episode.** Median notional is 208.97 USDT; the 90th percentile is 3104.32, and the 99th is 18190.22. The full empirical distribution is sampled without trimming. An aggregate CEX clip is not necessarily a complete customer order and does not identify non-toxic flow.

Customer intentions arrive through a Poisson process calibrated to expected offered volume of 100,000 or 1 million USDT per hour. Sizes are bootstrapped from that earlier distribution; directions are independently 50/50. Each customer draws an individual lognormal cost budget, with median 20, 50, 100 or 200 bps and log-standard-deviation 0.5. A trade fills only if its all-in quote meets that customer's budget. Actual offered and filled volumes vary by seed, and every concentration receives the same intentions and budgets within a paired case.

These are explicit assumptions about Dusk demand. The observed CEX volume does not establish pool capture, customer fee tolerance, clustered arrivals or directional predictability. The test contains 820 primary replays, 120 arbitrage-interval checks and 75 replays with less starting capital: 1,015 total.

## With enough fee-tolerant demand, 10× leads the shortlist
**At 1 million expected offered volume and a 100-bps median budget, 10× has the highest mean trading P&L in all five episodes.** Each displayed value averages five independent demand seeds, using 1 million initial capital and 1-second arbitrage opportunities. In the quiet hour, 5× earns 2317.98 USDT and 10× earns 2402.70. In the extreme-volatility hour they earn 2118.01 and 2555.14, respectively.

The quiet-hour mean advantage is small: 10× beats 5× in only three of five seeds there. Across all five episodes it wins 21 of 25 paired seed runs. Those are Monte Carlo comparisons under specified demand assumptions, not real-world confidence intervals or probabilities.

**Trading P&L includes every executed customer and arbitrage trade: ** LP fees plus signed execution edge at the contemporaneous external reference. Price-change gains and losses on inventory between trades are removed using an independent ledger. Do not interpret these values as total unhedged LP returns, and do not subtract LVR again.

## Cheap execution demands can reverse the ranking
**High concentration needs customers who will actually pay its resulting quotes.** On the −8.08% hour with 1 million expected offered volume, a 20-bps median budget leaves 3× at -5.22 USDT and 10× at -56.36. With a 100-bps median budget, those become 1298.33 and 2020.88.

The plotted comparison keeps the same historical hour, expected order volume, sizes, seed set and fee parameters; it changes customer willingness to accept execution cost. Individual budgets are heterogeneous, so a 20-bps median does not forbid every trade above 20 bps. A portion of customers have larger budgets.

High demand also does not guarantee that Dusk beats v1: at 1 million offered volume and a 50-bps median budget in that hour, v1 earns 333.70 USDT versus 323.93 at 10×. Across the entire 40-cell primary demand grid, 10× leads 20 cells, 3× nine, 5× eight and v1 three. The grid is not weighted by estimated market frequencies.

## Smaller initial pools can improve trading profit per capital under the tested demand
**The matched-capital check preserves the same absolute customer opportunities.** Each Dusk pool is resized to match v1's 10,000 USDT buy at the initial cold state. Required capital is approximately726,961 USDT at 3×, 624, 195 at 5× and563,311 at 10×, versus 1 million for v1. These are cold-start matches; they differ from the earlier warm-state capital figures because the volatility fee is initially zero.

The table uses 1 million expected offered volume, a 100-bps median budget and the same five seeds. It reports mean net trading bps relative to each pool's own initial capital. Every concentration is replayed on its actual resized curve; no P&L is merely scaled from the equal-capital result. 10× has the highest mean trading P&L per capital in all five of these matched-capital cases.

This does not establish equal risk or execution throughout the hour. Later prices, fee signals and quotes diverge, and the smaller pools receive more offered volume relative to their capital. The comparison is conditional on being offered the same absolute intentions.

## The main tradeoff survives the tested timing change
**Arbitrage timing is checked using identical seeds.** The latency extension compares opportunities every 1, 5 and 30 seconds while continuing to observe the same 1-second reference prices and processing customer intentions at their scheduled boundaries. The comparison uses the first three seeds, 1 million expected offered volume and 100-bps median budgets for every interval.

For those matched seeds, 5× leads in the quiet hour at all three intervals; 10× leads in the other four hours at all three intervals. This also illustrates why comparing five seeds at 1 second with only three at a slower interval would be misleading. The stored per-seed rows allow exactly paired comparisons.

The numerical optimizer now checks both the uncapped and capped outward-fee regimes. A coarse grid missed a second profit peak in an extreme-volatility replay. The revised solver agrees with an independent1,600-point search on four full extreme-hour replays and 60 randomized off-center opportunities. This is research-model computation; no on-chain compute-unit measurement is claimed.

## Advance 10× for strong demand, retain 3× and 5× as conditional alternatives
**The evidence favors a conditional shortlist.** 10× is the leading candidate when offered volume is substantial and customers accept the fee schedule. 3× provides the lower-exposure alternative when demand or fee tolerance is weak. 5× remains an intermediate candidate, including the quiet-hour runs where its advantage over 10× depends on the order sample.

Before choosing a live parameter or automatic controller, the controlling unknown is the target market's actual demand response: order sizes, available alternative quotes, willingness to pay, directional clustering and arrival intensity. Historical CEX prices and clip sizes do not measure those quantities for Dusk.

An adaptive controller should subsequently be evaluated against these fixed baselines on the same opportunities, including any price movement or value transfer caused by changing amplification. This run does not implement that controller or change protocol parameters.

## Data and accounting checks passed, with a defined scope
**The archives are checksum verified and the selected price paths reconcile.** The source check covers44,640 minute rows, 345, 600 second rows and238,579 prior-day aggregate clips. It checks timestamp units, continuous unique bar keys, OHLC bounds, finite prices, volumes and clip identifiers. All 300 selected minute closes and quote volumes reconcile with the corresponding second rows.

Every replay reconciles fee components, customer and arbitrage trading P&L, offered/fill counts, and an independent endpoint inventory ledger. Displayed means are recomputed in SQLite from sums and seed counts. The full raw seed rows, source hashes, source URLs, cost-budget assumptions and executed notebook cells accompany the report.

Limits: one CEX price proxy, five selected independent hours, five demand seeds, and unobserved Dusk customer demand. No regime-frequency weighting, annualized return, continuous-month performance, subsecond execution, strategic transaction splitting, integer program parity, recentering, adaptive amplification, fee compounding, hLP or borrowing is claimed. All monetary values are USDT quote units; no USD peg conversion is performed.

## Exact lookup

Mean one-hour net trading P&L in USDT, 1 million capital, 1 million expected offered volume, 100-bps median budget, five seeds.

| Historical episode | v1 |3× |5× |10× |
|---|---:|---:|---:|---:|
|Quiet|1140.02|1938.15|2317.98|2402.70|
|Typical|1214.62|2036.49|2370.43|2614.37|
|Volatile|1277.62|1698.61|1875.62|2148.58|
|Extreme|1188.67|1832.42|2118.01|2555.14|
|Largest move|1086.57|1298.33|1773.59|2020.88|

## Reproduce

```sh
python3 analysis/capital-efficiency/fetch_market_data.py
python3 analysis/capital-efficiency/shortlist_replay.py --workers 4
python3 analysis/capital-efficiency/shortlist_report.py
```

Public data documentation: https://github.com/binance/binance-public-data/blob/master/README.md. Archive URLs/checksums: `market-data/manifest.json`. Raw results: `shortlist_replays.csv`; summaries: `shortlist_summary.csv`; checks: `shortlist_validation.json`; data QA: `shortlist_data_quality.json`.
