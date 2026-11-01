# Validation report

> **Historical sealed DH2 validation snapshot; not a current-runtime
> validation.** The package-level assumptions and locked profiles apply only to
> this experiment.

## Overall assessment: native economic comparison complete

The prior missing native-controller comparison is now resolved. The benchmark independently replays Curve Twocrypto-NG v2.1's fixed-parameter swap, dynamic fee, internal EMA, gross-`xcp_profit` gate, and price-scale update beside current DH2's Dusk-native divergence/volatility fees, retained surcharge, and funded center controller.

Under the LP-net fee-adjusted ledger, neither system clears every native acceptance rule at the primary equal-execution lock across routes, flows, the top-ranked policy shortlist, pool scales, volume levels, and one-minute cadence.

For the same profiles selected on fee-adjusted synthetic return, the gross development-history outcome is diagnostic only: Fixed routing favors Curve by 0.12 bps per 12 hours; Elastic routing favors Curve by 0.08 bps per 12 hours. It is not a gross universal-winner claim.

Fixed routing's median fee-adjusted result favors Dusk by 0.06 bps per 12 hours. Elastic routing's median fee-adjusted result favors Dusk by 0.03 bps per 12 hours.

The locked-profile LP-return-only result at the primary equal-execution invariant match favors **Current DH2 + Dusk controller** by **0.05 bps per 12 hours** at the all-cell median. No system clears the stricter 1-bp, same-direction materiality threshold in both routes. Fixed and elastic route medians are +0.06 bps and +0.03 bps; positive values favor Dusk. This ranks LP return only. Execution cost, captured flow, tracking, and the stricter universal gate remain separate.

**Dusk synthetic product-readiness gate: fail.** The selected DH2 profile requires its own `rho = 2.069`; the common Curve–DH2 comparison relaxation cannot make it ready.

Those are simulation results under the declared ledger and acceptance gates. They are **not** claims of full deployed-protocol equivalence or forecasts of Jupiter-routed volume.

## Validation status by comparison layer

| Comparison level | Status | Supported use |
|---|---|---|
| Invariant only | Ready | Compare quote shape and one-jump LP loss at matched aggregate execution or matched jump risk. |
| Controlled system | Ready with modeled-flow caveat | Isolate invariant shape under one common Dusk fee, internal EMA, retention, and center controller. |
| Native system | Ready as a fixed-liquidity economic replay | Compare the two modeled native fee/controller systems across the canonical historical and sensitivity matrices. |
| Full deployment equivalence | Not claimed | Liquidity-event hooks, parameter ramps, Curve's liquidity-operation-triggered and once-per-24-hours-gated claim operation, live router flow, and Dusk integration/compute are outside this economic ranking. |

The selected native profiles are Curve `curve_grid_c01_f02` (controller=pinned_v21; EMA=10.0167m; step=1.46bp; allowance=2ppm; fees=base_like_flat; mid=5bp; out=5bp; fee_gamma=0.01) and Dusk `grid_c09_f10` (EMA 60m; threshold 200bp; step 50bp; interval 5m; dynamic cap 0bp; div 0; vol 0; vol half-life 15m). The original route-level training envelope was infeasible for the concentrated pair. The smallest common normalized widening was 206.9% (`rho = 2.069`). Original-envelope pass: Curve **no**, Dusk **no**. Because `rho > 1`, bounded fraction guardrails cease to bind and cost ceilings are widened by more than 2×; this shared rho governs only the fair Curve-versus-DH2 profile lock, not Dusk product readiness. Inside that eligible comparative envelope, each system first minimizes its selected profile's own required rho, then maximizes worse-route fee-adjusted return utility among ties before development-history scoring. The selected Dusk profile independently fails its original product-readiness envelope. Its own minimum normalized widening is 206.9% (`rho = 2.069`). Curve's comparative rho neither blocks nor rescues this Dusk-only gate, so the profile is not product-ready on synthetic training. CPMM is locked separately under the same own-rho-first order as `grid_c08_f10` (independent CPMM candidate; EMA 30m; threshold 100bp; step 25bp; interval 5m; dynamic cap 0bp; div 0; vol 0; vol half-life 15m). The independently selected CPMM also fails its original route-service envelope. Its separate normalized widening is 170.5% (`rho = 1.705`). It remains an economic comparator, but it is not eligible for a ship or product-ready recommendation.

The common Curve–DH2 rho controls only comparative profile matching. Dusk's own rho controls Dusk synthetic product readiness. CPMM's rho controls only its fallback qualification. `marginal_tracking_stale_fraction` is the readiness staleness metric; selected-order costs are finite-size service measurements, and `depth_degradation_fraction` is a raw 1%-TVL depth diagnostic rather than stale-price evidence.

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

The selected rows show each original-envelope result and the separate depth diagnostic:

| Selected training profile | Original pass | Elastic capture | Fixed p90 selected-order cost | Elastic p90 selected-order cost | Fixed marginal stale | Elastic marginal stale | Fixed depth degradation | Elastic depth degradation |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Curve | no | 62.3% | 280.71 bps | 281.51 bps | 6.6% | 1.4% | 87.5% | 87.5% |
| Current DH2 | no | 58.7% | 306.86 bps | 281.83 bps | 6.2% | 0.7% | 77.1% | 77.1% |
| Dusk CPMM | no | 51.4% | 270.47 bps | 269.52 bps | 9.7% | 2.1% | 9.0% | 1.7% |

## Symmetric concentrated-search, independent CPMM-search, and promotion integrity

Curve and concentrated Dusk each complete the same **150 × 128 = 19,200-cell** decision matrix. CPMM independently completes its own **150 × 128 = 19,200-cell** matrix. Quick output, partial matrices, stale search fingerprints, a promotion whose source files changed, a missing promoted search winner, or a canonical locked profile outside its top-ranked policy shortlist all fail the audit.

| Family | Full search | Search winner | Promoted | Canonical locked profile | Locked configuration |
|---|---:|---|---:|---|---|
| Curve Twocrypto-NG v2.1 | 150 × 128 = 19,200 | `curve_grid_c01_f02` | 5 | `curve_grid_c01_f02` (source rank 1) | A=1000; gamma=0.000461233; mid/out=5.0/5.0 bps; EMA=10.0167m; min step=1.5 bps |
| Current DH2 + Dusk controller | 150 × 128 = 19,200 | `grid_c09_f10` | 5 | `grid_c09_f10` (source rank 1) | peak=2000; fade=2.13483e-05; EMA=60m; threshold/step=200.0/50.0 bps; dynamic cap=0.0 bps |
| Independently selected Dusk CPMM | 150 × 128 = 19,200 | `grid_c08_f10` | 5 | `grid_c08_f10` (source rank 1) | peak=0; fade=0; EMA=30m; threshold/step=100.0/25.0 bps; dynamic cap=0.0 bps |

## Canonical matrix and integrity gates

| Output count | Required and observed rows |
|---|---:|
| `profile_rows` | 15 |
| `training_rows` | 1,920 |
| `held_out_rows` | 480 |
| `paired_rows` | 240 |
| `cpmm_rows` | 240 |
| `cpmm_matched_policy_ablation_rows` | 240 |
| `counterfactual_rows` | 480 |
| `neighborhood_rows` | 480 |
| `scale_rows` | 288 |
| `cadence_rows` | 96 |
| `volume_sensitivity_rows` | 288 |
| `basis_sensitivity_rows` | 1,440 |
| `fee_split_sensitivity_rows` | 960 |

- Historical score set: **15** non-overlapping SOLUSDT windows, each with 24-hour burn-in and 12-hour scoring.
- Flow set: **4** regimes (`benign, low_volume_stuck, mixed, toxic_delayed`), with arrival rates `benign=0.4/min, low_volume_stuck=0.08/min, mixed=0.4/min, toxic_delayed=0.4/min`, two seeds, and fixed plus elastic routing.
- Primary comparison basis: equal aggregate invariant execution; this is the only basis with controllers and fees selected for it and the only basis used by the universal gate.
- Locked-controller diagnostics: nearest full quote profile, equal one-jump risk, and the pinned v2.1 deployed invariant; none is reoptimized and none can reverse the universal label.
- Robustness: common-envelope-eligible top-ranked policy shortlists (Curve Twocrypto-NG v2.1: 5/5 top-ranked policies and 240/240 rows eligible; Current DH2 + Dusk controller: 2/5 top-ranked policies and 96/240 rows eligible), three pool sizes, one-minute cadence, 5%-to-400% locked-profile daily volume, center-movement-off, an independently selected Dusk CPMM product comparator, a same-policy zero-concentration ablation, and Dusk base-fee ownership from 0% through 100%.
- All decision metrics are finite across every underlying replay row: **true**.
- Maximum native ledger reconciliation error: **2.344e-07** quote-value units.
- Controller reads of the external reference price: **0**.
- Arbitrage events still profitable after the bounded same-slot cascade across burn-in and scoring: **0**.
- Arbitrage-search arithmetic, overflow, or solver failures across burn-in and scoring: **0**.
- Deterministic Curve cascade witness: **20 fills, 19 price-scale recenters, no profitable follow-up**; Curve emergency ceiling **64**, shared DH2/CPMM ceiling **16**.
- Frozen native history SHA-256: **`3dd9ed4250aa65ff3f7abdb26eb1f93c604ac3e7aee4d95f788f7b2545d55222`**.

The report generator rejects smoke output, a non-canonical status, a missing matrix, any mismatched CSV row count, or a profile-selection mismatch.

## Post-lock sealed confirmation

**Sealed confirmation: PASS.** The locked development classification was **neither system**; the post-lock sealed core classification is **neither system**. They agree under the predeclared route, flow-route, and CPMM core thresholds.

| Route | Development Dusk − Curve LP return | Sealed Dusk − Curve LP return | Development cell wins | Sealed cell wins | Development window wins | Sealed window wins |
|---|---:|---:|---:|---:|---:|---:|
| Fixed | +0.06 bps | +0.09 bps | 58.3% | 63.3% | 66.7% | 60.0% |
| Elastic | +0.03 bps | +0.05 bps | 56.7% | 60.8% | 60.0% | 60.0% |

The sealed audit re-hashes the immutable profile lock, all three complete search summaries, the replay engine, history CSV and metadata, and both replay output ledgers. It reconstructs every paired delta from the 720 underlying result rows, recomputes the sealed summary diagnostics, and requires all 720 paired rows. The confirmation status can be **PASS** or **FAIL** without modifying the locked selection; evidence-integrity failure prevents report generation entirely. During the evidence audit, the first sealed-summary producer was found to take the maximum signed accounting residual instead of the maximum absolute residual. The economic result and paired ledgers were unchanged; their preserved hashes are `7017fccfb0c059eaa230dafa916f852fda528ff5ce8b22b01f8317b0b8900109` and `408b1078298b1a6a7c1c38a575556bf4009ccb2c6faffff5ffd4a19218171cb8`. The corrected producer is covered by a mixed-sign regression, and the true maximum absolute residual is 2.081e-07, below the 1.0e-05 limit.

## Economic and service evidence

### Development-history routing results

| Route | Dusk − Curve gross LP return (fee-adjusted-selected profiles) | Dusk − Curve fee-adjusted LP return | Selected-order cost delta | Captured-volume delta | Marginal-tracking stale delta |
|---|---:|---:|---:|---:|---:|
| Fixed | -0.12 bps | +0.06 bps | -2.89 bps | +0.0 pp | -0.7 pp |
| Elastic | -0.08 bps | +0.03 bps | -1.62 bps | +2.5 pp | +0.0 pp |

### Fee-adjusted flow-by-route tails

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

### Alternate invariant-match diagnostics with locked controllers

| Matching basis | Route | Median Dusk − Curve fee-adjusted LP return | Bottom-decile mean | p90 execution regression | p10 capture advantage |
|---|---|---:|---:|---:|---:|
| Equal Jump Risk | Elastic | +0.01 bps | -11.01 bps | -0.09 bps | -1.4 pp |
| Equal Jump Risk | Fixed | +0.05 bps | -11.89 bps | +0.95 bps | +0.0 pp |
| Nearest Full Profile | Elastic | -0.04 bps | -18.52 bps | -0.59 bps | -0.0 pp |
| Nearest Full Profile | Fixed | -0.05 bps | -18.84 bps | -1.71 bps | +0.0 pp |
| V21 Deployed Invariant | Elastic | +0.16 bps | -1.23 bps | +16.07 bps | -28.9 pp |
| V21 Deployed Invariant | Fixed | +0.56 bps | -0.70 bps | +28.68 bps | +0.0 pp |

### Locked-profile daily-volume transfer

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

### Revenue-ownership sensitivity

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

## Acceptance predicates that did not pass

Every fee-adjusted predicate must pass before a candidate receives the universal-winner label. Gross diagnostics are not eligible for that label because profile selection used fee-adjusted return. A failed predicate means “no universal winner”—not an economic tie.

- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** selected profile passes original training service envelope
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** both concentrated profiles pass original training service envelope
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** training service is deployment ready
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** common training service relaxation is zero
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** both routes median advantage at least 1bp
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** both routes window win fraction at least 70pct
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** both routes bottom decile mean not below minus 1bp
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** both routes tail service regressions within limits
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** every flow route bottom decile mean not below minus 1bp
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** every flow route tail service regressions within limits
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** top ranked policy shortlist win fraction at least 60pct
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** every top ranked policy shortlist flow route has no reversal
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** pool scale win fraction at least two thirds
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** every scale flow route has no reversal or tail regression
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** every one minute flow route has no reversal or tail regression
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** every locked profile volume flow route has no reversal or tail regression
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** beats cpmm every locked profile volume flow route without reversal or tail regression
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** beats dusk cpmm both routes median advantage at least 1bp
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** beats dusk cpmm both routes window win fraction at least 70pct
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** beats dusk cpmm both routes bottom decile mean not below minus 1bp
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** beats dusk cpmm both routes tail service regressions within limits
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** beats dusk cpmm every flow route median has no return reversal
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** beats dusk cpmm every flow route bottom decile not below minus 1bp
- **Fee-adjusted LP universal gate / Current DH2 + Dusk controller:** beats dusk cpmm every flow route tail service within limits
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** selected profile passes original training service envelope
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** both concentrated profiles pass original training service envelope
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** training service is deployment ready
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** common training service relaxation is zero
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** both routes median advantage at least 1bp
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** both routes window win fraction at least 70pct
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** both routes bottom decile mean not below minus 1bp
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** both routes tail service regressions within limits
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** every flow route median has no return reversal
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** every flow route bottom decile mean not below minus 1bp
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** every flow route tail service regressions within limits
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** top ranked policy shortlist win fraction at least 60pct
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** every top ranked policy shortlist flow route has no reversal
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** pool scale win fraction at least two thirds
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** every scale flow route has no reversal or tail regression
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** one minute cadence win fraction at least two thirds
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** every one minute flow route has no reversal or tail regression
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** every locked profile volume flow route has no reversal or tail regression
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** beats cpmm every locked profile volume flow route without reversal or tail regression
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** beats dusk cpmm both routes median advantage at least 1bp
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** beats dusk cpmm both routes window win fraction at least 70pct
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** beats dusk cpmm both routes bottom decile mean not below minus 1bp
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** beats dusk cpmm both routes tail service regressions within limits
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** beats dusk cpmm every flow route median has no return reversal
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** beats dusk cpmm every flow route bottom decile not below minus 1bp
- **Fee-adjusted LP universal gate / Curve Twocrypto-NG v2.1:** beats dusk cpmm every flow route tail service within limits

## Corrections retained from the earlier analysis

- Raw `A`, `gamma`, `peak_depth`, and `fade_scale` are never compared directly; configurations are matched by observable quote service or jump risk.
- Arbitrage maximizes positive after-fee profit and is skipped when unprofitable.
- Claimable token fees remain LP wealth but are excluded from executable reserves; retained surcharge is counted once.
- Dusk's production 10-ppm output haircut is neutral to protected-Q recenter capacity and is not misclassified as retained surcharge.
- Curve's liability carries burn-in `xcp_profit_a` into the scoring window; Dusk fee ownership applies to base fees only, while dynamic surcharge remains LP-owned.
- Volatility is driven by successful AMM trade shocks, divergence surcharge is outward-only, and neither controller can see the external price.
- `realized_arbitrage_extraction_proxy_value` is only simulated realized arbitrage profit; it is not labeled as complete LVR.
- Paired differences are formed per identical window, flow, seed, and route before aggregation.

## Independent Curve reference and source boundary

The reference is pinned to Twocrypto-NG v2.1.0 commit `5cbe558902402e8fcb331463089db65fc56c11f9`. Bidirectional fixture quotes match the observed Ethereum deployment within one smallest six-decimal unit. Curve source code, tests, constants, comments, and implementation structure are neither copied nor vendored.

The replay covers fixed-parameter swaps, dynamic fees, EMA, the gross-profit gate, and price-scale moves. It does not simulate liquidity-event hooks, `A`/`gamma` ramps, or the admin-fee claim operation that a liquidity action may trigger and that is time-gated to at most once per 24 hours. The modeled 25%-of-gross-`xcp_profit` liability represents LP economics without pretending to reproduce that operation schedule; no automatic daily claim transaction is assumed.

## Decision-impacting limitations

1. Historical reference prices are observed; order arrivals, informed-flow accuracy, whole-order router elasticity, arbitrage priority, and arbitrage latency remain modeled. Elastic routing is a venue-choice tournament against a stateless outside CPMM, not optimal split routing.
2. SOLUSDT is used as a SOL/USDC market-path proxy. Results should be retested with signed production intents, fills, competing quotes, and post-trade markouts before making a volume claim.
3. This is a fixed-liquidity, exact-input economic replay. Exact-output flow, deposits, withdrawals, lending, leverage, liquidation, hLP operations, parameter ramps, fee/EMA manipulation, transaction ordering attacks, and bad debt are excluded rather than populated with invented assumptions.
4. A universal-winner gate is deliberately stricter than a median ranking. If neither candidate clears every tail and robustness predicate, the report says neither is universally dominant while still showing the actual economic deltas.
5. The boundary-cell censor marks these selected scalar points: equal execution / Curve CryptoSwap, equal execution / Current DH2, equal jump risk / Curve CryptoSwap, equal jump risk / Current DH2. Their precise gaps and apparent direction are conditional on the declared domains; they are not global family optima.

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

The authoritative machine-readable decision is `results/native_summary.json`; `DECISION_REPORT.md`, this validation report, the notebook, and `artifact.json` derive from the same frozen outputs.
