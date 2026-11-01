# Curve CryptoSwap vs current DH2, with CPMM control

> **Historical sealed DH2 snapshot; “current DH2” means current within this
> experiment, not the repository runtime.** This report does not rank or
> validate the present nested tail-and-band Dusk implementation.

## Executive summary

**The completed comparison has distinct scalar, controlled-path, and native answers.** At the equal-execution scalar cross-section, **Current DH2** has the numerically lowest one-jump LP loss. At the equal-jump-risk scalar cross-section, **Current DH2** has the numerically lowest execution index. These are boundary-conditioned results from the declared sampled domains—not Pareto results or global family optima. Equal-execution LP-loss scalar leader status: **Current DH2** is **boundary-censored** within `peak_depth = 2…2,000; fade_scale = 1e-6…0.5`. Equal-jump-risk execution scalar leader status: **Current DH2** is **boundary-censored** within `peak_depth = 2…2,000; fade_scale = 1e-6…0.5`.

Current DH2 leads the CryptoSwap invariant by $2 at the synthetic paired-cell median. Current DH2 leads the CryptoSwap invariant by $167 at the historical paired-cell median. These paired medians use the common controller; they are path-dependent and do not establish a Pareto or global invariant winner.

On the native-controller replay, under the LP-net fee-adjusted ledger, neither system clears every native acceptance rule at the primary equal-execution lock across routes, flows, the top-ranked policy shortlist, pool scales, volume levels, and one-minute cadence.

Fixed routing's median fee-adjusted result favors Dusk by 0.06 bps per 12 hours. Elastic routing's median fee-adjusted result favors Dusk by 0.03 bps per 12 hours.

The locked-profile LP-return-only result at the primary equal-execution invariant match favors **Current DH2 + Dusk controller** by **0.05 bps per 12 hours** at the all-cell median. No system clears the stricter 1-bp, same-direction materiality threshold in both routes. Fixed and elastic route medians are +0.06 bps and +0.03 bps; positive values favor Dusk. This ranks LP return only. Execution cost, captured flow, tracking, and the stricter universal gate remain separate.

**Sealed confirmation: PASS.** The locked development classification was **neither system**; the post-lock sealed core classification is **neither system**. They agree under the predeclared route, flow-route, and CPMM core thresholds.

**Dusk synthetic product-readiness gate: fail.** The selected DH2 profile requires its own `rho = 2.069`; the common Curve–DH2 comparison relaxation cannot make it ready. The common Curve–DH2 rho remains a comparative profile-lock diagnostic, and the CPMM has its own fallback qualification. The common-controller paired medians favor current DH2 over the CryptoSwap invariant in both synthetic and historical classes. Dusk product readiness still comes only from the independent Dusk gate above.

For the same profiles selected on fee-adjusted synthetic return, the gross development-history outcome is diagnostic only: Fixed routing favors Curve by 0.12 bps per 12 hours; Elastic routing favors Curve by 0.08 bps per 12 hours. It is not a gross universal-winner claim. Gross and fee-adjusted results answer different questions: the former describes the already fee-adjusted-selected profiles before protocol/manager revenue liabilities, while the latter is the ledger used to select profiles and test the universal claim.

## What is—and is not—being ranked

| Comparison level | What is held common | Supported conclusion |
|---|---|---|
| Invariant only | Same marginal-price states, marked TVL, trade-size/direction grid, zero fees, and no recentering; the jump test also starts from identical reserves | At the equal-execution scalar cross-section, **Current DH2** has the numerically lowest one-jump LP loss. At the equal-jump-risk scalar cross-section, **Current DH2** has the numerically lowest execution index. These are boundary-conditioned results from the declared sampled domains—not Pareto results or global family optima. Equal-execution LP-loss scalar leader status: **Current DH2** is **boundary-censored** within `peak_depth = 2…2,000; fade_scale = 1e-6…0.5`. Equal-jump-risk execution scalar leader status: **Current DH2** is **boundary-censored** within `peak_depth = 2…2,000; fade_scale = 1e-6…0.5`. |
| Controlled system | Same order tape, internal EMA, Dusk fee policy, retained-surcharge budget, and funded center controller | Current DH2 leads the CryptoSwap invariant by $2 at the synthetic paired-cell median. Current DH2 leads the CryptoSwap invariant by $167 at the historical paired-cell median. These paired medians use the common controller; they are path-dependent and do not establish a Pareto or global invariant winner. |
| Native system | Curve Twocrypto-NG v2.1 fixed-parameter fee/EMA/profit-gated price-scale controller versus current DH2 with Dusk divergence/volatility fees, retained surcharge, and funded recentering | Neither system is the fee-adjusted universal winner under the primary equal-execution gate. The separate locked-profile LP-return leader states economic direction even when the strict universal label is null. Gross return is diagnostic only. |

The native result is a model result, not a live-volume forecast. It uses frozen historical prices and policy-blind modeled order tapes; it does not prove what a deployed pool will earn under future router flow.

At equal aggregate execution service:

- Curve CryptoSwap: **86.43 bps** — 0.42% above Current DH2
- Current DH2: **86.07 bps** — numerically lowest in this sampled cross-section

At equal one-jump LP risk:

- Curve CryptoSwap: **64.70 bps** — 1.46% above Current DH2
- Current DH2: **63.77 bps** — numerically lowest in this sampled cross-section

On paired 12-hour controlled-system paths, current DH2 leads the CryptoSwap invariant by $2 at the synthetic paired-cell median. Current DH2 leads the CryptoSwap invariant by $167 at the historical paired-cell median. These paired medians use the common controller; they are path-dependent and do not establish a Pareto or global invariant winner.

## The comparison now holds trader service or LP risk constant

Raw `A`, `gamma`, `peak_depth`, and `fade_scale` values are not comparable across invariants. The corrected benchmark uses two independent iso-comparisons:

1. **Equal execution:** identical geometric-mean exact-input quote cost across price states from 0.67× to 1.50× the center, 1–2,000-bps marked-TVL orders, and both directions.
2. **Equal jump risk:** identical mean loss versus hold after profit-maximizing zero-fee arbitrage over nine one-step price jumps.

The declared domains are read from `results/parameter_matching.csv`: **Curve CryptoSwap:** paper A = 0.1…1,000; gamma = 1e-8…0.199; **Current DH2:** peak_depth = 2…2,000; fade_scale = 1e-6…0.5. A boundary-censored scalar winner is only the best selected point inside that declared search and is never labeled a global family optimum.

The former “full profile match” could not match every wing: its nearest current-DH2 solution retained a worst-point impact error of 69.3%. The report therefore does not pretend the Curve and current-DH2 shapes are interchangeable.

| Basis | Curve | Current DH2 |
|---|---:|---:|
| Jump loss at equal execution | 86.43 bps | 86.07 bps |
| Execution cost at equal jump risk | 64.70 bps | 63.77 bps |
| Center impact at equal execution | 0.348 bps | 1.955 bps |
| Equal-execution selection boundary-censored? | Yes — boundary-censored | Yes — boundary-censored |
| Equal-jump-risk selection boundary-censored? | Yes — boundary-censored | Yes — boundary-censored |

### Concrete center quotes

The following zero-fee exact-input examples use the selected equal-execution reference configurations and a balanced **$2.0M marked-TVL pool**. Cost and quoted output are computed independently in each token direction and then averaged; the inverse-price direction is not reconstructed by subtracting its cost. “Quoted output value” marks both input and output at the pre-trade center, so the difference from input is curve impact rather than a token-price forecast.

| Model | Exact-in size | Mean two-direction center cost | Quoted output value |
|---|---:|---:|---:|
| Curve CryptoSwap | $2,000 (10 bps TVL) | 0.010 bps | $2,000.00 |
| Curve CryptoSwap | $20,000 (100 bps TVL) | 0.348 bps | $19,999.30 |
| Curve CryptoSwap | $100,000 (500 bps TVL) | 153.479 bps | $98,476.99 |
| Curve CryptoSwap | $200,000 (1000 bps TVL) | 913.594 bps | $182,561.04 |
| Current DH2 | $2,000 (10 bps TVL) | 0.012 bps | $2,000.00 |
| Current DH2 | $20,000 (100 bps TVL) | 1.955 bps | $19,996.09 |
| Current DH2 | $100,000 (500 bps TVL) | 186.732 bps | $98,150.11 |
| Current DH2 | $200,000 (1000 bps TVL) | 894.077 bps | $182,916.24 |
| CPMM | $2,000 (10 bps TVL) | 19.980 bps | $1,996.01 |
| CPMM | $20,000 (100 bps TVL) | 198.039 bps | $19,607.84 |
| CPMM | $100,000 (500 bps TVL) | 954.545 bps | $90,909.09 |
| CPMM | $200,000 (1000 bps TVL) | 1833.333 bps | $166,666.67 |

At an illustrative **$150/SOL** center, the 100-bps-of-TVL row means either selling 133.333 SOL or 20,000 USDC:

| Model | Sell 133.333 SOL | Sell 20,000 USDC |
|---|---:|---:|
| Curve CryptoSwap | 19,999.30 USDC (0.348 bps) | 133.328699 SOL (0.348 bps) |
| Current DH2 | 19,996.09 USDC (1.954 bps) | 133.307274 SOL (1.955 bps) |
| CPMM | 19,607.84 USDC (196.078 bps) | 130.718954 SOL (200.000 bps) |

The execution index is deliberately broad; the center row shows why one scalar is insufficient. At the 100-bps center probe, **Curve CryptoSwap** is numerically lowest in this boundary-conditioned equal-execution fit. The full wings and Pareto frontier remain separate comparisons.

## The recomputed Curve–DH2 Pareto frontier contains both ranked families

Curve CryptoSwap has 23 sampled frontier points, Current DH2 has 14 sampled frontier points. The zero-concentration CPMM control has 1 sampled frontier point. Frontier membership means only that a family has at least one non-dominated sampled configuration; point count is not a quality score. Scalar cross-section leadership does not prove Pareto or global dominance.

This frontier is boundary- and grid-conditioned evidence, not a global proof over every continuous parameter value.

## Paired controlled-path economics are derived pair by pair

The primary path replay applies the same Dusk fee, internal EMA, retention gate, and funded center controller to each concentrated design. Each model receives the same externally denominated user order tape. Arbitrage sizes maximize actual after-fee profit and are skipped below the profit threshold.

| Competitor minus CryptoSwap invariant | Synthetic median | Synthetic wins | Historical median | Historical wins | Historical window-median wins |
|---|---:|---:|---:|---:|---:|
| Current DH2 | $2 | 53.1% | $167 | 70.0% | 60.0% |

Current DH2 leads the CryptoSwap invariant by $2 at the synthetic paired-cell median. Current DH2 leads the CryptoSwap invariant by $167 at the historical paired-cell median. These paired medians use the common controller; they are path-dependent and do not establish a Pareto or global invariant winner.

## The corrected ledger removes the old double counting

LP wealth is:

```text
(executable base + claimable base) × external price
+ executable quote
+ claimable quote
```

Base fees leave executable reserves and remain token-denominated LP wealth. Only retained dynamic surcharge enters reserves and can create protected recentering budget. It is counted once, inside reserves.

The Dusk fee reference now mirrors the actual ordering:

- volatility decays before a trade and is updated from the successful AMM price shock;
- divergence charges only outward reserve movement;
- restorative flow pays no divergence surcharge;
- divergence has priority under the dynamic-fe cap;
- surcharge retention is binary;
- quote input, reserve credit, claimable fees, and retained principal reconcile exactly.

Protected liquidity is `Q = D / (2√center)` for the Curve and current-DH2 concentrated invariants. Changing the center with unchanged tokens has zero immediate marked-wealth cost. `Q` impairment is a controller-capacity diagnostic and is not subtracted a second time from LP return.

## Synthetic profile lock does not read development history

The original route-level training envelope was infeasible for the concentrated pair. The smallest common normalized widening was 206.9% (`rho = 2.069`). Original-envelope pass: Curve **no**, Dusk **no**. Because `rho > 1`, bounded fraction guardrails cease to bind and cost ceilings are widened by more than 2×; this shared rho governs only the fair Curve-versus-DH2 profile lock, not Dusk product readiness.

The selected Dusk profile independently fails its original product-readiness envelope. Its own minimum normalized widening is 206.9% (`rho = 2.069`). Curve's comparative rho neither blocks nor rescues this Dusk-only gate, so the profile is not product-ready on synthetic training.

The independently selected CPMM also fails its original route-service envelope. Its separate normalized widening is 170.5% (`rho = 1.705`). It remains an economic comparator, but it is not eligible for a ship or product-ready recommendation.

The same base guardrails apply independently to fixed and elastic routes. Hard gates—finite metrics, accounting error no greater than `1e-5`, and zero controller reads of the external price—never relax. For service only, Curve and concentrated Dusk use the smallest common normalized relaxation that admits at least one hard-valid profile from each candidate family. CPMM uses its own minimum relaxation and cannot change either concentrated profile or `comparison_pair_ready_on_training_service`. Dusk's readiness is computed separately from its selected profile's own required rho. Within every system, selection first minimizes the profile's own required rho, then maximizes worse-route LP-return utility among ties. Historical windows are not read.

The three rho values have different jobs: the **common Curve–DH2 rho** makes the comparative lock behaviorally fair; the **independent Dusk rho** alone decides Dusk's synthetic product-readiness status; and the **independent CPMM rho** qualifies the fallback comparator. Marginal-price tracking defines stale readiness. Selected-order execution cost is a finite-size service measure, while the raw 1%-TVL `depth_degradation_fraction` is shown only as a depth diagnostic and never relabeled as stale price.

| Per-route synthetic guardrail | Original threshold |
|---|---:|
| Median captured volume | ≥ 50.0% |
| Median selected-order execution cost (finite-size service) | ≤ 25.00 bps |
| p90 selected-order execution cost (finite-size service) | ≤ 100.00 bps |
| Median marginal-price tracking stale time | ≤ 75.0% |
| Worst-flow median capture | ≥ 25.0% |
| Worst-flow p10 capture | ≥ 10.0% |
| Worst-flow median marginal-price tracking stale time | ≤ 90.0% |
| Worst-flow p90 selected-order execution cost | ≤ 150.00 bps |

The selected rows show the original-envelope result and the separate depth diagnostic:

| Selected training profile | Original pass | Elastic capture | Fixed p90 selected-order cost | Elastic p90 selected-order cost | Fixed marginal stale | Elastic marginal stale | Fixed depth degradation | Elastic depth degradation |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Curve | no | 62.3% | 280.71 bps | 281.51 bps | 6.6% | 1.4% | 87.5% | 87.5% |
| Current DH2 | no | 58.7% | 306.86 bps | 281.83 bps | 6.2% | 0.7% | 77.1% | 77.1% |
| Dusk CPMM | no | 51.4% | 270.47 bps | 269.52 bps | 9.7% | 2.1% | 9.0% | 1.7% |

## Symmetric concentrated searches and an independent CPMM search remove profile-budget asymmetry

Curve and concentrated Dusk each receive exactly **150 initial controller/fee profiles × 128 synthetic cells = 19,200 decision cells** before promotion. CPMM receives its own independent **150 × 128 = 19,200-cell** search under the same hard-validity-first, synthetic-only budget. The report accepts neither quick output nor a partially evaluated family.

Promotion is an explicit provenance boundary, not an implicit search rewrite. Each top-ranked policy shortlist is bound to its search summary, stage outputs, winner, and fingerprint. The canonical native selector may choose only a source-ranked promoted policy under the applicable service envelope; it cannot invent a profile outside that source-bound shortlist.

| Family | Full search | Search winner | Promoted | Canonical locked profile | Locked configuration |
|---|---:|---|---:|---|---|
| Curve Twocrypto-NG v2.1 | 150 × 128 = 19,200 | `curve_grid_c01_f02` | 5 | `curve_grid_c01_f02` (source rank 1) | A=1000; gamma=0.000461233; mid/out=5.0/5.0 bps; EMA=10.0167m; min step=1.5 bps |
| Current DH2 + Dusk controller | 150 × 128 = 19,200 | `grid_c09_f10` | 5 | `grid_c09_f10` (source rank 1) | peak=2000; fade=2.13483e-05; EMA=60m; threshold/step=200.0/50.0 bps; dynamic cap=0.0 bps |
| Independently selected Dusk CPMM | 150 × 128 = 19,200 | `grid_c08_f10` | 5 | `grid_c08_f10` (source rank 1) | peak=0; fade=0; EMA=30m; threshold/step=100.0/25.0 bps; dynamic cap=0.0 bps |

The full configurations—not only display labels—are bound by the promotion snapshot, the canonical replay fingerprint, and the pre-history sealed lock. This is why the search winner and the canonical locked profile are shown separately.

## Native replay: the strict decision is generated from routes, tails, and robustness—not one median

Under the LP-net fee-adjusted ledger, neither system clears every native acceptance rule at the primary equal-execution lock across routes, flows, the top-ranked policy shortlist, pool scales, volume levels, and one-minute cadence.

For the same profiles selected on fee-adjusted synthetic return, the gross development-history outcome is diagnostic only: Fixed routing favors Curve by 0.12 bps per 12 hours; Elastic routing favors Curve by 0.08 bps per 12 hours. It is not a gross universal-winner claim.

### Locked-profile LP-return leader at the primary equal-execution match

The locked-profile LP-return-only result at the primary equal-execution invariant match favors **Current DH2 + Dusk controller** by **0.05 bps per 12 hours** at the all-cell median. No system clears the stricter 1-bp, same-direction materiality threshold in both routes. Fixed and elastic route medians are +0.06 bps and +0.03 bps; positive values favor Dusk. This ranks LP return only. Execution cost, captured flow, tracking, and the stricter universal gate remain separate.

Positive deltas below favor Dusk. The **gross** column is an outcome for the profiles selected on fee-adjusted return; it compares LP wealth before the modeled external revenue liabilities and is not a separate universal ranking. The **fee-adjusted** column subtracts the liability accrued during the scored window while preserving liability already present at the score boundary. Execution-cost deltas are for the selected route: native venue for fixed routing and the cheaper of AMM or outside venue for elastic routing.

| Route | Dusk − Curve gross LP return (fee-adjusted-selected profiles) | Dusk − Curve fee-adjusted LP return | Selected-order cost delta | Captured-volume delta | Marginal-tracking stale delta |
|---|---:|---:|---:|---:|---:|
| Fixed | -0.12 bps | +0.06 bps | -2.89 bps | +0.0 pp | -0.7 pp |
| Elastic | -0.08 bps | +0.03 bps | -1.62 bps | +2.5 pp | +0.0 pp |

The selected native profiles are Curve `curve_grid_c01_f02` (controller=pinned_v21; EMA=10.0167m; step=1.46bp; allowance=2ppm; fees=base_like_flat; mid=5bp; out=5bp; fee_gamma=0.01), Dusk `grid_c09_f10` (EMA 60m; threshold 200bp; step 50bp; interval 5m; dynamic cap 0bp; div 0; vol 0; vol half-life 15m), and independently selected Dusk CPMM `grid_c08_f10` (independent CPMM candidate; EMA 30m; threshold 100bp; step 25bp; interval 5m; dynamic cap 0bp; div 0; vol 0; vol half-life 15m). The original route-level training envelope was infeasible for the concentrated pair. The smallest common normalized widening was 206.9% (`rho = 2.069`). Original-envelope pass: Curve **no**, Dusk **no**. Because `rho > 1`, bounded fraction guardrails cease to bind and cost ceilings are widened by more than 2×; this shared rho governs only the fair Curve-versus-DH2 profile lock, not Dusk product readiness. The selected Dusk profile independently fails its original product-readiness envelope. Its own minimum normalized widening is 206.9% (`rho = 2.069`). Curve's comparative rho neither blocks nor rescues this Dusk-only gate, so the profile is not product-ready on synthetic training. The independently selected CPMM also fails its original route-service envelope. Its separate normalized widening is 170.5% (`rho = 1.705`). It remains an economic comparator, but it is not eligible for a ship or product-ready recommendation. Within each system's eligible envelope, the lock first minimizes the profile's own required rho; only then does it maximize **worse-route** synthetic utility among ties. The utility is `0.8 × median fee-adjusted LP return + 0.2 × p10 return`, and all profiles are locked before development-history replay. Top-ranked-policy-shortlist robustness applies to the two concentrated candidates and excludes policies outside their common service envelope (Curve Twocrypto-NG v2.1: 5/5 top-ranked policies and 240/240 rows eligible; Current DH2 + Dusk controller: 2/5 top-ranked policies and 96/240 rows eligible). The table above uses 15 profile-blind development historical windows, 4 modeled flow regimes, two seeds, and both routes. These windows were visible during benchmark development and are not the sealed confirmation set. The primary equal-execution acceptance rule requires both routes to pass independently and also checks bottom-decile returns, service tails, every flow-by-route cell, the eligible top-ranked policy shortlist, the independently selected Dusk CPMM product comparator, three pool scales, one-minute cadence, and locked-profile volume transfer. Alternate invariant matches are diagnostics only.

## Sealed post-lock history tests replication without reopening selection

**Sealed confirmation: PASS.** The locked development classification was **neither system**; the post-lock sealed core classification is **neither system**. They agree under the predeclared route, flow-route, and CPMM core thresholds.

The development columns use the 15 profile-blind historical windows that were visible while the benchmark was being developed. The sealed columns use 15 new mechanically scheduled windows from May 2025 through May 2026, downloaded only after the Curve, DH2, and CPMM profiles, search sources, matrix, and replay-engine hashes were locked. Positive return deltas favor Dusk.

| Route | Development Dusk − Curve LP return | Sealed Dusk − Curve LP return | Development cell wins | Sealed cell wins | Development window wins | Sealed window wins |
|---|---:|---:|---:|---:|---:|---:|
| Fixed | +0.06 bps | +0.09 bps | 58.3% | 63.3% | 66.7% | 60.0% |
| Elastic | +0.03 bps | +0.05 bps | 56.7% | 60.8% | 60.0% | 60.0% |

The binary confirmation applies the existing route, flow-route, and CPMM return/service thresholds to the locked profiles. It is intentionally narrower than the full native universal gate because the sealed matrix does not rerun the top-ranked policy shortlist, pool scales, volume levels, or one-minute cadence. Alternate matching bases are outside both gates and remain locked-controller diagnostics. A sealed **FAIL** blocks confirmation; it does not select a replacement, retune a parameter, or silently change `results/native_summary.json`.

## Flow-by-route tails prevent pooled medians from deciding the native winner

The next table shows Dusk-minus-Curve fee-adjusted outcomes at the decision grain. A positive return advantage helps Dusk; positive selected-order cost or marginal-tracking stale regression hurts Dusk. This prevents a benign or elastic-flow median from hiding failure under informed flow or fixed routing.

| Flow | Route | Median Dusk − Curve LP return | Bottom-decile mean | p90 selected-order cost regression | p10 capture advantage | p90 marginal-tracking stale regression |
|---|---|---:|---:|---:|---:|---:|
| Benign | Elastic | +0.06 bps | -10.80 bps | +1.32 bps | -1.4 pp | +1.4 pp |
| Benign | Fixed | +0.09 bps | -9.98 bps | +1.66 bps | +0.0 pp | +0.7 pp |
| Low Volume Stuck | Elastic | +0.02 bps | -4.63 bps | +0.09 bps | -2.1 pp | +0.7 pp |
| Low Volume Stuck | Fixed | +0.04 bps | -7.29 bps | +0.70 bps | +0.0 pp | +0.1 pp |
| Mixed | Elastic | +0.09 bps | -10.98 bps | +0.13 bps | -1.0 pp | +1.4 pp |
| Mixed | Fixed | +0.06 bps | -9.88 bps | +1.95 bps | +0.0 pp | +0.7 pp |
| Toxic Delayed | Elastic | +0.03 bps | -9.73 bps | +0.26 bps | -0.3 pp | +4.9 pp |
| Toxic Delayed | Fixed | +0.11 bps | -12.95 bps | +1.21 bps | +0.0 pp | +0.8 pp |

## Alternate invariant matches are locked-controller transfer diagnostics

Native results are repeated with the nearest full quote profile, equal one-jump LP risk, and the pinned v2.1 deployment invariant while keeping the controllers and fees selected on the primary equal-execution match. Those controllers are **not reoptimized** for an alternate basis. Reversals therefore diagnose transfer sensitivity but cannot grant, revoke, or reverse the universal label.

| Matching basis | Route | Median Dusk − Curve fee-adjusted LP return | Bottom-decile mean | p90 execution regression | p10 capture advantage |
|---|---|---:|---:|---:|---:|
| Equal Jump Risk | Elastic | +0.01 bps | -11.01 bps | -0.09 bps | -1.4 pp |
| Equal Jump Risk | Fixed | +0.05 bps | -11.89 bps | +0.95 bps | +0.0 pp |
| Nearest Full Profile | Elastic | -0.04 bps | -18.52 bps | -0.59 bps | -0.0 pp |
| Nearest Full Profile | Fixed | -0.05 bps | -18.84 bps | -1.71 bps | +0.0 pp |
| V21 Deployed Invariant | Elastic | +0.16 bps | -1.23 bps | +16.07 bps | -28.9 pp |
| V21 Deployed Invariant | Fixed | +0.56 bps | -0.70 bps | +28.68 bps | +0.0 pp |

## Locked profiles are stress-tested from 5% to 400% daily volume

The next table holds each selected profile and its arrival frequency fixed while changing notional throughput. It is a transfer test, not a per-volume reoptimization. The primary equal-execution gate evaluates every underlying flow-route cell; this compact view shows the median of cell medians and the worst service/return tails at each volume.

| Comparison | Daily volume / pool | Median of flow-route medians | Worst bottom-decile mean | Worst p90 execution regression | Worst p10 capture advantage | Worst p90 marginal-stale regression |
|---|---:|---:|---:|---:|---:|---:|
| Curve − CPMM | 5% | -0.06 bps | -39.60 bps | +0.85 bps | -16.6 pp | +3.5 pp |
| Curve − CPMM | 25% | -0.11 bps | -40.02 bps | +25.04 bps | -13.6 pp | +10.8 pp |
| Curve − CPMM | 100% | +0.10 bps | -60.48 bps | +92.20 bps | -26.3 pp | +7.3 pp |
| Curve − CPMM | 400% | -10.37 bps | -76.33 bps | +79.69 bps | -10.1 pp | -1.4 pp |
| Dusk − CPMM | 5% | +0.01 bps | -40.10 bps | -0.94 bps | -0.0 pp | +3.8 pp |
| Dusk − CPMM | 25% | +0.02 bps | -40.46 bps | +7.51 bps | -10.4 pp | +9.4 pp |
| Dusk − CPMM | 100% | -0.05 bps | -42.28 bps | +91.68 bps | -25.6 pp | +10.8 pp |
| Dusk − CPMM | 400% | +3.97 bps | -67.46 bps | +92.39 bps | +0.0 pp | +20.8 pp |
| Dusk − Curve | 5% | +0.07 bps | -0.62 bps | +0.76 bps | -0.4 pp | +2.1 pp |
| Dusk − Curve | 25% | -0.06 bps | -0.78 bps | +0.97 bps | -3.5 pp | +4.2 pp |
| Dusk − Curve | 100% | +0.47 bps | -3.18 bps | +37.09 bps | -4.6 pp | +17.0 pp |
| Dusk − Curve | 400% | +14.90 bps | -70.78 bps | +121.03 bps | -9.8 pp | +39.2 pp |

## Revenue ownership can change LP ranking without changing a single quote

The primary fee-adjusted ledger gives Dusk's manager 5% of the base fee, gives the Dusk dynamic surcharge entirely to LPs, and applies Curve's modeled effective admin liability of 25% of gross `xcp_profit` growth. Dusk's total base-fee share is then revalued from 0% to 100% without replaying trades because the split does not change quotes, reserves, surcharge retention, or controller decisions.

| Dusk base-fee revenue share | Route | Median Dusk − Curve LP return | p10 return delta | Dusk cell-win share |
|---:|---|---:|---:|---:|
| 0% | Elastic | +0.06 bps | -3.38 bps | 60.0% |
| 0% | Fixed | +0.09 bps | -4.83 bps | 59.2% |
| 5% | Elastic | +0.03 bps | -3.42 bps | 56.7% |
| 5% | Fixed | +0.06 bps | -4.91 bps | 58.3% |
| 25% | Elastic | -0.08 bps | -3.67 bps | 36.7% |
| 25% | Fixed | -0.09 bps | -5.19 bps | 35.8% |
| 100% | Elastic | -0.65 bps | -6.18 bps | 5.0% |
| 100% | Fixed | -0.93 bps | -6.89 bps | 5.8% |

This sensitivity isolates ownership policy from AMM mechanism performance. It must not be used to claim that a revenue split itself improves quoting or reduces adverse selection.

## Recentring, the CPMM comparator, and concentration ablation are distinct

Enabling native center movement changed fee-adjusted LP return by a median +0.00 bps for Curve and +0.00 bps for Dusk. The corresponding selected-execution-cost changes were -0.13 bps and +0.00 bps; tracking-RMSE changes were -14.47 bps and +0.00 bps. Fees, observations, and Dusk retention remain active in the off arm, so this isolates the economic consequence of suppressing center moves rather than disabling the whole controller. These are paired on the same development-history windows, flows, seeds, and fixed/elastic routes.

In a deterministic Curve regression witness, one same-slot event required **20 profitable fills and 19 price-scale recenters** before no profitable follow-up remained. It converged without a numerical failure. The benchmark therefore uses a Curve-local emergency ceiling of 64 fills while the shared DH2/CPMM ceiling remains 16. This witness shows that Curve's controller can reopen arbitrage after a fill; 20 is neither a claimed canonical-matrix maximum nor an on-chain execution count.

The **product comparator** selects its own Dusk fee/controller profile on the same synthetic training matrix but under a separate route-service relaxation, without reading development history or changing the Curve/DH2 lock. The independently selected CPMM also fails its original route-service envelope. Its separate normalized widening is 170.5% (`rho = 1.705`). It remains an economic comparator, but it is not eligible for a ship or product-ready recommendation. Against that independently selected CPMM, current DH2 changed median LP return by -0.01 bps, selected-order execution cost by -8.48 bps, captured volume by +0.0 pp, and marginal-tracking stale time by -0.7 pp. This comparison gates every universal concentrated-candidate claim even when the CPMM itself is not eligible for a product-ready recommendation.

The **matched-policy ablation** instead preserves the selected current-DH2 fee and center-controller parameters and changes only `peak_depth = fade_scale = 0`. Under that isolated concentration removal, current DH2 changed median LP return by -0.01 bps, selected-order execution cost by -8.48 bps, captured volume by +0.0 pp, and marginal-tracking stale time by -0.7 pp. This arm diagnoses the marginal effect of concentration; it does not select the product comparator or enter winner predicates.

## What these results say about Dusk

**Controlled paths favor current DH2.** The common-controller paired medians favor current DH2 over the CryptoSwap invariant in both synthetic and historical classes.

**Use the native decision gate for product-level claims.** Under the LP-net fee-adjusted ledger, neither system clears every native acceptance rule at the primary equal-execution lock across routes, flows, the top-ranked policy shortlist, pool scales, volume levels, and one-minute cadence. Fixed routing's median fee-adjusted result favors Dusk by 0.06 bps per 12 hours. Elastic routing's median fee-adjusted result favors Dusk by 0.03 bps per 12 hours. Separately, at the equal-execution scalar cross-section, **Current DH2** has the numerically lowest one-jump LP loss. At the equal-jump-risk scalar cross-section, **Current DH2** has the numerically lowest execution index. These are boundary-conditioned results from the declared sampled domains—not Pareto results or global family optima. Equal-execution LP-loss scalar leader status: **Current DH2** is **boundary-censored** within `peak_depth = 2…2,000; fade_scale = 1e-6…0.5`. Equal-jump-risk execution scalar leader status: **Current DH2** is **boundary-censored** within `peak_depth = 2…2,000; fade_scale = 1e-6…0.5`.

**Dusk synthetic product-readiness gate: fail.** The selected DH2 profile requires its own `rho = 2.069`; the common Curve–DH2 comparison relaxation cannot make it ready. The locked-profile LP-return-only leader is **Current DH2 + Dusk controller** at the primary equal-execution match; quote service and the stricter universal label remain separate. **Sealed confirmation: PASS.** The locked development classification was **neither system**; the post-lock sealed core classification is **neither system**. They agree under the predeclared route, flow-route, and CPMM core thresholds. The comparative Curve–DH2 rho, Dusk product rho, and CPMM fallback rho must not be merged into one deployment label.

The supported next decision is:

1. keep the invariant-only, common-controller, and native-system conclusions separate;
2. retain current DH2 as the concentrated Dusk candidate and exact CPMM as its zero-concentration control;
3. retune or redesign the Dusk fee/controller profile until its own fixed and elastic route-service envelope passes with `rho = 0`;
4. then compare current DH2 with Curve at the chosen LP-risk and quote-surface target and rerun the native gate;
5. keep the measured locked-profile LP-return leader explicit and CPMM as the required zero-concentration comparator, without importing either comparator's rho into Dusk readiness.

## Sharper current-DH2 fade: hard rejected

The tested shape kept the same two public operator parameters—`peak_depth` and `fade_scale`—and fixed its exponent at two inside the protocol:

```text
legacy: A_eff = (peak_depth / 2) / (1 + u / fade_scale)
sharp:  A_eff = (peak_depth / 2) / (1 + (u / fade_scale)^2)
```

Both have identical center depth, and they cross at `u = fade_scale`. The sharp profile retains **more** depth before that shoulder and decays faster only after it. This is not an unqualified reduction in off-center liquidity.

The candidate is rejected before economic ranking because it violates invariant uniqueness. At `peak_depth = 323`, `fade_scale = 0.5`, `x = 6.776596755`, and `y = 0.036891674`, the same reserve state has **3 valid invariant roots**: 2.082136870, 2.486864718, 2.874788264. A swap solver can therefore select different reserve branches for the same state.

The diagnostic same-parameter calculation at that point reports an execution-index change of +1.36 bps and a one-jump-loss change of -14.49 bps. Those numbers are explicitly **non-decision-grade**: branch ambiguity invalidates the quote surface they summarize. The sharp profile is an analytical float reference only; it has not been Rust/fixed-point validated and should not be implemented.

A second experiment removed that feedback by measuring distance as `r = |x-y| / (2√xy)` and using the protocol-fixed profile `A_eff = (peak_depth / 2) / (1 + (r / fade_scale)^4)`. The D-independent p4 repair has one invariant root in every scanned state, but this remains a sampled analytical-float check rather than a fixed-point proof.

- Jump loss at equal execution: current DH2 **86.07 bps**; p4 **104.08 bps** — 20.9% higher, favoring current DH2.
- Execution index at equal jump risk: current DH2 **63.77 bps**; p4 **76.71 bps** — 20.3% higher, favoring current DH2.

The direct D-dependent sharper fade fails safety. Both like-for-like scalar objectives favor current DH2, so the p4 repair is economically rejected in this experiment. The evidence-derived production status is `keep_current_dh2_fade`.

The controlled-path result is not a product-readiness endorsement and does not establish either ranked invariant as globally superior. Current DH2's public invariant knobs are stated as `peak_depth` (additional center marginal-depth multiplier) and `fade_scale`; legacy internal `A = peak_depth / 2`. `peak_depth = 0` together with `fade_scale = 0` is exact CPMM. Fees, EMA half-life, adjustment threshold, and recenter velocity remain controller parameters, not invariant parameters.

## Scope and remaining uncertainty

The controlled-system frozen history contains five highest-range 720-minute SOLUSDT windows from one-minute Binance archives; USDT is a proxy for USDC. The native replay separately uses 15 development historical windows after burn-in and 15 post-lock sealed windows for confirmation. Synthetic and historical classes are never pooled into one winner statistic. Within a controlled-system class, paths, the benign/toxic-delayed flow regimes, and two seeds receive equal weight.

Historical prices are real; user and arbitrage arrivals are modeled. The quote-routing share is an equal-intent static tournament, not a Jupiter volume forecast. Real signed router orders, fills, latency, fees, and post-trade markouts could change the commercial conclusion.

The controlled replay uses one $2.0M pool size, one 25%-of-TVL daily volume level, one fee/EMA/recenter configuration, and two flow regimes. It does not establish controlled-system sensitivity across pool size, volume, or controller settings. The invariant economic ranking uses exact-input orders from 1 to 2,000 bps; exact-output remains a safety/shape diagnostic rather than a ranking weight. The native system separately tests locked-profile volume transfer from 5% to 400% of pool value per day.

The economic ranking excludes implementation readiness and IP. Current DH2's continuous Python equation is cross-validated against Rust; production exact-input quotes include a conservative 10-ppm haircut. That haircut is not charged against current DH2 in the pure-math ranking. The native Dusk replay does charge it and mirrors the Rust retention/recenter transition ordering, but EMA, Q, center, and candidate calculations remain analytical floating point rather than a multi-trade bit-exact Rust trace. The declared matching domains, read directly from `parameter_matching.csv`, are **Curve CryptoSwap:** paper A = 0.1…1,000; gamma = 1e-8…0.199; **Current DH2:** peak_depth = 2…2,000; fade_scale = 1e-6…0.5. CPMM remains a separate zero-concentration control. Curve's bounds come from pinned Twocrypto-NG v2.1 parameter checks converted to paper-A normalization.

The equal-execution result matches one geometric-mean impact index, not the full quote surface. The boundary-cell censor marks these selected scalar points: equal execution / Curve CryptoSwap, equal execution / Current DH2, equal jump risk / Curve CryptoSwap, equal jump risk / Current DH2. Their precise gaps and apparent direction are conditional on the declared domains; they are not global family optima.

The Curve native reference independently implements the published Twocrypto-NG v2.1 fixed-parameter swap, dynamic fee, EMA, `xcp_profit` gate, and price-scale update equations. It is validated against the pinned deployment fixture and published equations without copying or vending Curve source code. It does **not** reproduce liquidity-event hooks, parameter ramps, or the admin-fee claim that is triggered by a liquidity operation and time-gated to at most once per 24 hours; there is no automatic daily transaction in the model or the described protocol behavior. Curve's already-accrued liability is carried across the score boundary, and the v2.1 claim is represented as a 25%-of-gross-`xcp_profit` effective liability rather than as a simulated claim operation.

The native development replay covers 15 non-overlapping 12-hour SOLUSDT scoring windows after 24-hour burn-in, four modeled flow regimes, two seeds, fixed and elastic routing, and 5-minute base cadence. The sealed replay repeats the locked-profile primary matrix on 15 post-lock windows. Arrival rates are `benign=0.4/min, low_volume_stuck=0.08/min, mixed=0.4/min, toxic_delayed=0.4/min`; the low-volume/stuck regime therefore reduces both total notional and the frequency of trade-driven controller observations. Primary-gate sensitivities add the top-ranked policy shortlist, three pool sizes, 1-minute cadence, 5%-to-400% locked-profile daily volume, center-movement-off, CPMM, and alternate Dusk base-fee ownership. Three alternate invariant matches remain non-optimized diagnostics. Historical prices are observed; arrivals, trader information, outside routing, and arbitrage behavior remain modeled assumptions. No external price is available to either controller.

## Reproduce

```bash
cd analysis/cryptoswap-vs-dh2
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 benchmark.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 dusk_controller_search.py --workers 4
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 curve_controller_search.py --workers 4
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 cpmm_controller_search.py --workers 4
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 promote_controller_profiles.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 native_benchmark.py --workers 4
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 audit_native_outputs.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 native_sealed_validation.py --materialize-lock
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 fetch_sealed_native_history.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 native_sealed_validation.py --workers 4
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 audit_native_outputs.py --final-report
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 build_report.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 -m unittest discover -s tests -v
$NODE_BIN $REPORT_BUILDER --input artifact.json --output report.html
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 finalize_report_html.py report.html
```

The controlled historical snapshot SHA-256 is `d3c963ea52eeb19c925e1cde1c98c6e945608f0d1020e3b62b5c4acf05eb73ee`. The native window snapshot identity and pinned Curve release are recorded in `data/native_history_metadata.json` and `CURVE_REFERENCE.md`. Machine-readable results are in `results/`.
