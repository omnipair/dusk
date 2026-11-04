# Paper-rule OP-AMM pricing prototype (17 October 2026)

**Status: independent research prototype, not a Dusk integration or a production parameter choice.** Developed on 16–17 October 2026 and recovered for publication on 3 November 2026, this folder implements exact-input and exact-output swap formulas for the [OP-AMM paper's Price-Tracking (PT) and Geometric-Average (GA) constructions](https://arxiv.org/html/2609.33799v1). It predates Dusk's integrated OP-GAMM implementation and does not validate that later implementation. The Horus observation-layer design was separate research and is not packaged here.

## What is packaged

The package contains the analytical pricing model, its nine tests, saved historical and synthetic results, the standalone SBF probe and its saved numerical checks. It contains no raw exchange datasets, oracle-provider histories, wallets or native Dusk replay harness.

The historical comparison uses **historical market data with a simulated feed delay**. It does not measure historical authenticated oracle-report latency or document an actual provider incident. The repeated incorrect-report test is also synthetic. The later bounded-adjustment study is a separate experiment; it is not needed to substantiate this earlier delayed-feed comparison.

The market-data and native-harness dependencies needed to rerun particular commands are explicitly listed under [Reproduce and audit](#reproduce-and-audit). The mathematical tests and checks of saved SBF outputs can run without those omitted dependencies.

## Decision supported by this prototype

Do not select full oracle pass-through solely from an ideal-feed replay. In the present selected SOL/USDT stress windows, full PT has no arbitrage when its reference is the *same contemporaneous Bybit mid* used for the execution book. Under an assumed one-minute report delay, its locally Dusk-depth-matched variant has **$16,619** aggregate LP shortfall versus holding across three separate $1m-starting-capital weeks. A 50% GA variant with the same launch-point reserve slope has **−$2,521** aggregate shortfall in those same modeled weeks. These are path-dependent scenario outputs, not expected losses or evidence that 50% is optimal. A separate repeated ±4% erroneous-report exercise extracts **$78,902** from PT versus **$39,502** from the matched GA after 20 base-neutral cycles. This shows that pool base inventory returning to its start does not bound cash loss.

At the time of this study, the next *implementation feasibility* candidate was GA at λ=0.5 with matched reserve slope, retaining PT as the ideal-feed benchmark. The experiment does not justify selecting λ=0.5 or the power-law tail as a market default: authenticated oracle timing, actual Dusk/hLP accounting, boundary behavior and integrated SBF compute require separate validation. The paper does not prescribe λ. A later governance change also needs an explicit repricing transition.

## Mathematical specification

Let `P0` be the launch normalization price, `x` the base reserve, `y` the quote reserve, `X=P0*x`, `Y=y`, `r=Y/X`, and `S=oracle_reference/P0`. `X`, `Y`, `r`, and `S` are positive; `r` and `S` are dimensionless. The implemented marginal quote is

```text
P_marginal = P0 * A * r^β,   A = S^λ.
```

For PT, λ=1 and β=α when the paper's baseline is `φ(r)=r^α`. For GA, `0<λ<1` and β=`(1−λ)α`. This is the paper's log-price formulation after putting the two physical tokens in consistent units. Every tested rule is continuously differentiable for positive reserves, strictly increasing and surjective in `r`, and has oracle log-sensitivity `λ` in `[0,1]`. The fee is separate from this marginal price.

At a fixed oracle value the exact conditional invariant can be written, up to a monotone transform, as:

```text
β = 1:  K = A*ln(X) + ln(Y)
β ≠ 1:  K = Y^(1−β) + A*X^(1−β).
```

For `β=1`, quote input `q` after fee gives `X' = X * (Y/(Y+q))^(1/A)`; base input `b` after fee gives `Y' = Y * (X/(X+P0*b))^A`. For `β<1`, put `c=1−β`: quote input gives `X' = [(K−(Y+q)^c)/A]^(1/c)` and base input gives `Y' = [K−A*(X+P0*b)^c]^(1/c)`. Exact base output has the corresponding inverse formula in [model.py](model.py). Inputs to these formulas are **net of the pool fee**; the gross input is credited to reserves, so the retained fee increases the conditional invariant. An oracle update changes `A` and the quote without moving tokens; the next trade uses the new conditional invariant through the unchanged reserves.

The apples-to-apples **oracle-sensitivity** pair is `PT_full_local5x` `(λ,β)=(1,0.2)` versus `GA_50_local5x` `(0.5,0.2)`. The GA baseline uses `φ(r)=r^0.4`, making both rules have the same launch-point marginal price and reserve elasticity. The `*_local5x` label approximates Dusk's 5x *local* center depth. It is **not** Dusk's five-segment curve. The conventional same-CPMM-baseline pair (`PT_full`: `(1,1)` versus `GA_50_CPMM`: `(0.5,0.5)`) is also included, but it changes both oracle sensitivity and reserve depth. `GA_25_matched` and `GA_75_matched` show sensitivity around the 50% choice. `No_oracle_local5x` and `CPMM_no_oracle` are mathematical controls, not executable Dusk clones.

## Static quote check

All model pools start with 5,000 base and 500,000 quote at `P0=$100`, for $1m NAV, and a 3 bp retained input fee. The oracle then jumps 10% without a trade. The table shows a $10,000 quote-input purchase:

| Rule | New marginal price | Average execution price |
| --- | ---: | ---: |
| PT, local 5x slope | $110.00 | $110.453 |
| 50% GA, same local slope | $104.881 | $105.322 |
| No oracle, same local slope | $100.00 | $100.430 |

The **frozen native Dusk** 5x pure-yLP harness, with its own dynamic fees, quotes a $10,000 buy at **$100.435** at the original $100 reference. That near-match checks only the initial local slope and fee scale. It does not make the power-law model equivalent to Dusk's bounded five-segment curve or its hLP, lending, leverage, and revenue accounting. See [native_static.json](native_static.json). The original native harness is not packaged; its dependency is described below.

## Historical book-hedged replay

The benchmark streams all 30,240 saved one-minute SOL/USDT observations across the three previously selected June, October, and November 2025 stress weeks, using every fifth Bybit order-book snapshot (6,048 finite books). Each policy/week starts with a fresh balanced $1m pool. At each sampled minute, an external arbitrageur considers both trade directions and five fixed sizes ($500–$50,000 quote-equivalent), chooses the most profitable **single** pool trade, and hedges at the displayed Bybit book with a modeled 5 bp execution fee. One pool trade is allowed per sampled observation; there are no customer trades, borrowers, hLPs, or position liquidations. The pool fee is 3 bp retained. The feed is either the contemporaneous Bybit mid (`ideal`) or the prior one-minute Bybit mid (`lag60_assumed`). The latter is a synthetic delay, not observed oracle-provider latency. A nonexecuted $1,000 probe on each side measures quote quality after the arbitrage step.

Aggregate across the three independently reset weeks for the equal-local-depth rules:

| Feed | Rule | Arbitrages | Book-hedged arb profit | LP shortfall vs starting-token HODL |
| --- | --- | ---: | ---: | ---: |
| Ideal | PT full | 0 | $0 | $0 |
| Ideal | GA 50% | 2,700 | $6,760.50 | **−$7,350** |
| Ideal | No oracle | 3,955 | $29,326 | $6,217 |
| Assumed 60s lag | PT full | 2,440 | $6,052 | **$16,619** |
| Assumed 60s lag | GA 50% | 3,018 | $10,108 | **−$2,521** |
| Assumed 60s lag | No oracle | 3,955 | $29,326 | $6,217 |

Positive shortfall means LP underperformed holding its exact starting tokens; negative means it outperformed. Arbitrage profit and HODL shortfall are **different measures**: LPs also change inventory during the external price path, so one cannot infer one from the other. The GA rule induces more arbitrage than PT under a lagged feed but happens to leave more LP NAV on these paths. The no-oracle model has no dependence on feed delay by construction. The post-arbitrage $1,000 buy probe costs about 7 bp on average for all three local-depth rules; that is a **hypothetical quote**, not observed customer acceptance. Full per-week and same-CPMM-baseline rows are in [results.json](results.json).

## Repeated-error and compute checks

At a fixed true mark of $100, an attacker observes 20 sequential report pairs of $96 then $104, each with a fresh opportunity to buy base at the low report and sell the *same base quantity* at the high report. Each cycle chooses its best size from $500–$50,000 quote-equivalent; pool fees remain 3 bp. There is no external hedge or independent depth constraint. This is an intentionally severe, synthetic reporting-failure diagnostic, **not** a historical attack estimate or a globally optimized attacker. All 20 cycles are profitable for PT and matched GA; base inventory ends exactly where it began while pool cash falls by the attacker's quote profit. [results.json](results.json) contains each rule's outcome.

All analytical quote paths are bounded-branch, constant-work functions in the host prototype. A Python timing loop records roughly 1.0–1.3 μs per $1,000 quote including pool creation. The separate SBF probe below measures actual executable quote compute. The [paper's implementability appendix](https://arxiv.org/html/2609.33799v1) distinguishes a closed-form bonding curve from oracle-read costs. The `β<1` power-law conditional curves have finite reserve-axis intercepts; Dusk's current curve instead has nonzero full-range tails. Tail liquidity, maximum input, reserve exhaustion, and hLP integration remain unsolved.

## Standalone Solana SBF quote probe

[The probe](sbf-probe/src/lib.rs) implements both locally 5x-depth-matched rules at `β=0.2`: PT with full oracle sensitivity (`λ=1`) and GA with half sensitivity (`λ=0.5`). For exact-input swaps, it floors the fee-adjusted input, finds a nearby output with a bounded fixed-point binomial series, and accepts the final output only after a conservative integer invariant comparison using exact fifth roots in `U512`. A correction loop is capped at 16 atoms and fails closed if it cannot bracket the answer. No floating-point operation participates in the executable quote. This is a bounded research implementation, **not** a full-domain production library.

The probe accepts a deliberately narrow numerical domain: six-decimal base and quote, whole-dollar launch price `P0=$100` in these runs, positive reserves at most `10^15` quote atoms after the net input, and a 1e9-scale oracle price. It rejects a quote when the series expansion parameter exceeds 0.4. It only returns a quote; no oracle accounts, refresh instructions, transfer fees, Dusk invariant segments, hLP accounting, or token transfers are in the measured transaction. The `16`-atom correction bound and input domain are prototype restrictions. The exact-root and coefficient rounding make accepted outputs conservative for this conditional invariant. A production implementation must prove full-domain behavior and either support larger swaps or specify their rejection policy.

The [base grid](sbf-probe/results-sbf.json) has 100 quotes: five reference prices ($96–$110), five sizes ($1–$50,000 equivalent), both trade directions, and both rules, with 5,000 base and $500,000 quote in the pool at a 3 bp input fee. High-precision Decimal comparison found **zero overquotes and zero underquotes**. The [stress grid](sbf-probe/results-sbf-stress.json) has 3,080 valid quotes varying reference price ($50–$200), reserve ratio (0.5–2), fee (0–100 bp), and size (one input atom to $50,000 equivalent). It found **zero overquotes** and **one underquote of one output atom**. All 3,080 simulations completed within the Solana 1.4m CU limit. Median CU by operation:

| Quote operation | Base grid | Stress grid |
| --- | ---: | ---: |
| PT buy base | 430,158 | 393,139 |
| GA buy base | 450,670 | 413,373 |
| PT sell base | 514,066 | 482,620 |
| GA sell base | 533,180 | 503,479 |

The stress minimum of 472 CU in the buy path occurs when a one-atom gross input rounds to zero net input. The highest measured stress quote was 599,672 CU. These are standalone LiteSVM transaction CUs. They leave roughly 800k CU at the 1.4m limit for integration only in the single worst measured case, before external feed validation, Dusk logic, account reads, and token transfers. No integrated CU claim follows from this margin.

Reproduce with the installed Solana SBF toolchain, Node dependencies, and Python standard library:

```sh
cargo test --manifest-path analysis/opamm-pricing-2026-10-17/sbf-probe/Cargo.toml --offline
CARGO_NET_OFFLINE=true cargo build-sbf --manifest-path analysis/opamm-pricing-2026-10-17/sbf-probe/Cargo.toml --sbf-out-dir analysis/opamm-pricing-2026-10-17/sbf-probe/target/deploy
node analysis/opamm-pricing-2026-10-17/sbf-probe/bench.mjs
python3 analysis/opamm-pricing-2026-10-17/sbf-probe/check.py
node analysis/opamm-pricing-2026-10-17/sbf-probe/bench.mjs --stress
python3 analysis/opamm-pricing-2026-10-17/sbf-probe/check.py --stress
```

## Reproduce and audit

Publication verification on 3 November 2026 passed all nine Python model tests and both Rust probe tests. Replaying against the original local inputs reproduced all 54 historical rows, static probes and repeated-error outcomes exactly. Rebuilding the SBF probe reproduced its saved binary SHA256 and all 3,180 quote/CU/reference rows exactly. The saved numerical files remain unchanged. This verification used the omitted inputs locally; it does not make the published package self-contained.

From the repository root, the mathematical tests use only Python's standard library and require no market data:

```sh
python3 -B -m unittest discover -s analysis/opamm-pricing-2026-10-17 -p 'test_*.py' -v
```

The tests check conditional-invariant preservation at fixed reference, high-precision Decimal examples, input/output inverse consistency, fee behavior, no-trade oracle response, and base-neutral attacker cash conservation. The saved SBF outputs can also be checked independently without a build or market data:

```sh
python3 -B analysis/opamm-pricing-2026-10-17/sbf-probe/check.py
python3 -B analysis/opamm-pricing-2026-10-17/sbf-probe/check.py --stress
```

The checker rewrites the selected result JSON with recomputed reference fields and summary. Rebuilding the SBF probe using the commands above requires the Solana SBF toolchain, Cargo dependencies pinned by `sbf-probe/Cargo.lock`, and the repository's Node dependencies (`litesvm` and `@solana/web3.js`). Offline commands require dependencies already cached. The saved files identify the original binary hash; a different toolchain may produce a different binary or compute measurements. The saved compute figures are historical, quote-only measurements.

**Historical replay — inputs not included.** `benchmark.py` expects these artifacts in the original relative directory layout:

| Artifact | Bytes | SHA256 |
| --- | ---: | --- |
| `analysis/oracle-guided-amm-2026-10-16/data/reference_1m.csv` | 14,165,553 | `9d211e2cf36d3fea7d824990d0863ea4009ae887d0ae45d571a7a5addf9fcd09` |
| `analysis/incremental-liquidation-loss-2026-10-12/data/snapshots_1m.jsonl.gz` | 29,926,751 | `b1b5bfcc70b17b3114371a5257f2ca57908d3b3271f20018978b860e9064bdaa` |

The CSV points to the book file relative to its own directory. The benchmark consumes Bybit fields; the other venue columns in the original CSV do not drive these results. The original acquisition and reconstruction scripts are not packaged. [provenance.json](provenance.json) preserves verified artifact hashes, the selected dates and the 21 official upstream Bybit archive URLs and hashes. The [Bybit historical-data portal](https://www.bybit.com/derivatives/en/history-data) is the upstream entry point. The archives contain exchange observations, not oracle-provider reports. Exchange-emission timestamps are available; collector receipt timestamps are absent. Displayed depth is not a guarantee of fills.

This publication is therefore **not a self-contained reproduction of the historical replay**. Public availability does not establish unrestricted redistribution rights; check source terms and availability before acquiring or redistributing exchange data. Once the exact original inputs are restored, run:

```sh
python3 -B analysis/opamm-pricing-2026-10-17/benchmark.py
```

That command overwrites `results.json`. Economic outputs should reproduce from the hashed inputs; host timing is machine-dependent. The assumed delay remains separate from observed exchange-emission age.

**Native static comparison — optional, unbundled harness.** `native_probe.py` requires the original `oracle-guided-amm-2026-10-16` native JSON-lines adapter, analysis-patched Dusk source snapshot, configuration and built `native/target/debug/dusk-oracle-guided-replay`. None is packaged. [native_static.json](native_static.json) preserves the result, binary hash and original source-commit claim (`22114ad3606e768ed08df41d71e41ece3313d2c9`). That commit claim alone does not reconstruct the analysis patches, and rebuilding current Dusk main is not a substitute. This package cannot independently regenerate the native comparison.

This exercise cannot settle a tokenized stock, ETF, pre-stock, or commodity rule. SOL is a liquid-crypto proxy; the Bybit mid and book are the same venue; historical oracle receipts, market sessions, redemption, corporate actions, true borrower demand, and adversarial transaction ordering are absent. A final choice needs authenticated provider histories and a replay using the post-#45 Dusk/hLP accounting path, plus full-domain integer proofs and integrated SBF compute.
