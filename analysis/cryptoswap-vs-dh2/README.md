# CryptoSwap vs DH2 economic benchmark

> **Historical sealed snapshot — not a current-runtime validation.** This
> package models StableSwap-derived DH2 with configured fee caps, and the
> selected locked profiles disabled dynamic fees. It does **not** rank or
> validate the current Dusk hybrid with exact-CPMM tails and an uncapped
> outward divergence fee. See [LEGACY_NOTICE.md](LEGACY_NOTICE.md).

This package compares:

1. Curve CryptoSwap, independently reconstructed from published equations;
2. the current StableSwap-derived Dusk DH2 invariant;
3. the compact log-potential DH2 proposal;
4. CPMM as the zero-concentration reference.

It also retains two analytical sharper-fade experiments as explicit rejected
references: the direct exponent-2 profile fails invariant uniqueness, while a
D-independent hyperbolic p4 repair is materially worse at equal service/risk.

Current DH2 is parameterized only by `peak_depth` and `fade_scale`.
`peak_depth` is the added center marginal-depth multiplier; the legacy internal
coefficient is `peak_depth / 2`. Both values set to zero produce exact CPMM.

The benchmark answers an economic question only: trader execution, LP loss,
fee income, inventory exposure, and funded center behavior. It reports both a
common-controller comparison and a native-controller comparison. The report
generator reads the decision directly from `results/native_summary.json`; the
README does not hardcode a winner.

## Correct comparison layers

- **Invariant-only:** zero fees, external-marked TVL, exact price-space states,
  and both directions. Exact-in drives the ranking index; exact-out is reported
  as a shape diagnostic.
- **Equal execution:** each family is optimized to the same aggregate quote
  cost, then compared on one-jump LP loss and paired path economics.
- **Equal risk:** each family is optimized to the same one-jump LP loss, then
  compared on execution.
- **Pareto frontier:** the declared discrete parameter grids are retained; no
  sampled fitted point is promoted as a universal winner.
- **Controlled system:** each concentrated design receives the same Dusk fee,
  internal EMA, binary surcharge retention, protected-Q budget, and funded
  center controller.
- **Native system:** the independently implemented Twocrypto-NG v2.1
  fixed-parameter controller keeps its own fee, EMA, `xcp_profit` gate, and
  price-scale update, while current DH2 keeps Dusk's divergence/volatility
  fees, retained surcharge, and funded recentering.
- **Gross outcome for fee-adjusted-selected profiles:** LP wealth before external
  manager/admin fee liabilities, used as a diagnostic to separate mechanism
  behavior from revenue ownership—not as a second universal ranking.
- **Fee-adjusted LP economics:** Curve's modeled effective admin liability and
  Dusk's declared manager/protocol base-fee shares are applied to LP wealth.

The native universal-winner gate is intentionally strict and uses only the
primary **equal-execution** invariant match, because that is the basis on which
fees and controllers are selected. Fixed and elastic routing must pass
independently; every flow-by-route cell, eligible top-ranked-policy-shortlist
cell, pool scale, locked-profile daily-volume level, and one-minute cadence
check must avoid a material return or service reversal. Each concentrated
candidate must also beat the independently synthetic-selected Dusk CPMM product
comparator under the same route and flow-route return and service thresholds.
Alternate invariant matches keep the equal-execution controllers locked and are
non-optimized sensitivity diagnostics; they cannot grant, revoke, or reverse
the universal label. `null` means neither candidate clears the whole
fee-adjusted gate; it does not mean the two systems are economically equal.

The summary therefore reports a separate
`locked_profile_lp_return_leader_at_equal_invariant_execution_index`. It states
the LP-return direction and materiality for the locked primary profiles even
when the stricter universal winner is `null`. It is not a claim about execution,
capture, tracking, capital efficiency, or a globally optimal invariant family.

Profile selection is synthetic-only and route-safe. Numerical validity,
finite metrics, accounting error, and zero controller oracle reads are hard
gates. The three service relaxations have separate authority:

1. the common Curve–DH2 rho admits one hard-valid profile from each concentrated
   family for a fair comparative lock;
2. the independent Dusk rho alone determines whether the selected Dusk profile
   passes the original fixed- and elastic-route product-readiness envelope;
3. the independent CPMM rho qualifies only the fallback comparator.

Curve's required relaxation cannot block or rescue Dusk product readiness, and
CPMM cannot change either the comparative lock or the Dusk readiness result.
Within every system, the lock first minimizes that profile's own required rho,
then maximizes the same worse-route fee-adjusted return utility among ties. All
locks remain blind to held-out history. A Dusk or CPMM profile that needs positive
relaxation remains economically diagnosable but is not product-ready on the
synthetic service gate. Top-ranked-policy-shortlist robustness applies to the
concentrated candidates, uses only promoted policies admitted by their common
comparative envelope, and reports eligible and excluded policy/row counts. This
shortlist is selected by search rank; it is not a geometric parameter
neighborhood.

The committed `solusdt_native_windows.csv` is the development historical
holdout: profile selection does not read it, but it is visible during benchmark
development. The separate **sealed post-lock validation** is confirmatory. Its
five source months, stratum rule, profiles, flows, seeds, routes, and replay
engine hashes are frozen in `data/sealed_profile_lock.json` before those prices
are downloaded. Sealed results are diagnostics only and cannot select, replace,
or tune a profile.

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

`native_benchmark.py --quick` is a smoke test only. It writes to
`results/quick/`, carries `status: smoke_only`, and is never accepted by the
report generator. The canonical run writes atomically to `results/` and must
finish with `status: complete`.

No network access is required through the pre-lock audit. Inputs,
transformations, seeds, scenario weights, frozen development historical
snapshots, and the pinned Curve fixture are committed. The one-time sealed-
history fetch is the only network-dependent step in the full final-report run.

The controller searches are full decision runs. Curve and concentrated Dusk use
symmetric **150-profile × 128-cell** searches; CPMM receives its own independent
search with the same budget. `promote_controller_profiles.py` binds each search
winner and top-ranked policy shortlist to its source hashes before native replay.

The lock/fetch/replay commands above are the one-way sealed validation ceremony:

```bash
# 1. Reads the canonical promoted selection only; refuses if sealed data exists.
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 native_sealed_validation.py --materialize-lock

# 2. First and only network-dependent step; refuses without the valid lock.
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 fetch_sealed_native_history.py

# 3. Verifies lock, engine, CSV, and metadata hashes before the 720-cell replay.
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 native_sealed_validation.py --workers 4
```

The sealed month schedule is mechanical: `2025-05`, `2025-08`, `2025-11`,
`2026-02`, and `2026-05`. Each month uses the existing deterministic three-
strata rule. The benchmark, all three searches, promotion, native selection, and
pre-lock audit finish before sealed data are fetched. No selection or search
script imports or reads sealed artifacts.

## Outputs

- `DECISION_REPORT.md`: corrected answer and decision.
- `VALIDATION.md`: canonical matrix, accounting, source-boundary, and decision-gate audit generated from the same native summary.
- `artifact.json`: report-reader payload.
- `report.html`: portable interactive report generated from `artifact.json`.
- `native_controller_analysis.ipynb`: offline audit notebook over the committed
  native result files.
- `CURVE_REFERENCE.md`: clean-room equation, release, fixture, and scope record.
- `results/parameter_matching.csv`: equal-service, equal-risk, and nearest-shape
  parameter points.
- `results/pareto.csv`: complete execution-versus-jump-loss sweep.
- `results/path_results.csv`: controlled path ledger by model and paired cell.
- `results/paired_differences.csv`: per-cell DH2-minus-Curve comparisons.
- `results/regime_summary.csv`: distribution summaries without pooled-model
  medians.
- `results/routing.csv`: static equal-intent quote tournament.
- `results/safety.csv`: common numerical checks.
- `results/decay_profile.csv`: direct legacy-versus-exponent-2 fade shape.
- `results/same_parameter_deltas.csv`: fixed-exponent sweep using only
  `peak_depth` and `fade_scale`.
- `results/sharp_root_evidence.csv`: three-root hard-rejection witness.
- `results/hyperbolic_p4_comparison.csv`: equal-execution and equal-risk
  economic rejection of the D-independent repair.
- `results/hyperbolic_p4_root_scan.csv`: committed 41-state root scan.
- `results/scenario_weights.json`: explicit weighting policy.
- `results/controller_search/full/`, `curve_controller_search/full/`, and
  `cpmm_controller_search/full/`: complete Dusk, Curve, and independent CPMM
  search matrices and summaries.
- `data/dusk_controller_profiles.json`, `curve_controller_profiles.json`, and
  `cpmm_controller_profiles.json`: source-bound top-ranked policy shortlists
  promoted from those searches.
- `results/native_summary.json`: authoritative native decision, rule predicates,
  independent comparison/Dusk/CPMM selection policies, fee assumptions,
  robustness summaries, and validation counts.
- `results/native_profile_selection.csv`: synthetic training results and locked
  held-out profiles.
- `results/native_held_out.csv`: complete per-system historical replay ledger.
- `results/native_paired.csv`: Dusk-minus-Curve paired outcomes by window, flow,
  seed, and route.
- `results/native_matching_basis_sensitivity.csv`: non-optimized locked-
  controller diagnostics for nearest-shape, equal-risk, and pinned v2.1
  invariant matches. These rows do not enter the universal gate.
- `results/native_volume_sensitivity.csv`: locked-profile transfer from 5% to
  400% daily volume/pool at fixed arrival frequency; every underlying
  flow-route cell enters the primary equal-execution gate.
- `results/native_dusk_fee_split_sensitivity.csv`: accounting-only sensitivity
  to Dusk base-fee revenue ownership.
- `results/native_controller_counterfactual.csv`: native center movement on
  versus disabled pairs; fee and retention logic remain active.
- `results/native_cpmm_control.csv`: Dusk CPMM product comparator, independently
  selected on synthetic training under its own route-service relaxation and
  then replayed blind on held-out history. This is the CPMM arm used by winner
  gates, but it is not product-ready when that relaxation is positive.
- `results/native_cpmm_matched_policy_ablation.csv`: diagnostic CPMM arm using
  the selected current-DH2 fee and center controller with only `peak_depth` and
  `fade_scale` zeroed. It isolates concentration and never enters winner gates.
- `results/native_neighborhood.csv`, `native_scale_sensitivity.csv`, and
  `native_cadence_sensitivity.csv`: top-ranked-policy-shortlist, pool-size, and
  one-minute cadence robustness. The legacy filename does not imply a geometric
  parameter neighborhood.
- `data/sealed_profile_lock.json`: pre-history, self-fingerprinted Curve/DH2/CPMM
  lock bound to the canonical selection summary and exact replay engine.
- `data/solusdt_sealed_native_windows.csv`,
  `data/sealed_native_history_metadata.json`, and
  `data/sealed_native_history_manifest.json`: post-lock observations plus hashes
  binding both the CSV and metadata back to the profile lock.
- `results/sealed_validation/sealed_native_results.csv`,
  `sealed_native_paired.csv`, and `sealed_native_summary.json`: isolated
  confirmatory diagnostics. They contain no profile selection or recommendation.

## Metric interpretation

`execution_index_bps` is the geometric mean zero-fee execution cost across the
declared price, size, and direction grid. It is not a fee rate.

`jump_loss_index_bps` is mean loss versus hold after profit-maximizing zero-fee
arbitrage over nine one-step price jumps, in basis points of initial TVL.

`net_lp_pnl_vs_hold` includes executable reserves and token-denominated
claimable fees. Retained surcharge is already inside executable reserves and is
not added twice. Recenter impairment is a protected-capacity diagnostic, not an
additional subtraction from LP wealth.

`net_lp_return_after_protocol_fee_bps` is the native LP-net result after the
modeled revenue liability. `net_lp_return_vs_hold_bps` is the gross mechanism
result before that liability. The native report always presents both.

`marginal_tracking_stale_fraction` is the readiness staleness measure. It asks
whether the AMM's infinitesimal marginal price is outside the declared tracking
threshold, so finite trade impact and fees cannot make a correctly centered
CPMM stale by construction.

`selected_execution_cost_bps` is the realized finite-size service cost for the
selected route. `depth_degradation_fraction` is a separate raw 1%-TVL probe
diagnostic. Both remain visible, but `depth_degradation_fraction` is not a
stale-price predicate and does not replace marginal-price tracking in the
readiness gate.

`realized_arbitrage_extraction_proxy_value` is realized simulated arbitrage
profit, not a complete LVR measurement. It is never substituted for loss versus
hold or for the full continuous-time LVR counterfactual.

Historical prices are real; trade flow is modeled. Static best-quote share is
not a forecast of Jupiter volume.

The `elastic` route is whole-order venue choice against a stateless outside
CPMM, not optimal split routing. Native Dusk mirrors the Rust ordering of
neutral and retained checkpoints, stale-target probes, and funded recentering,
but its EMA, Q, center, and candidate arithmetic remains analytical floating
point rather than a multi-trade bit-exact Rust trace. Exact-output order flow,
liquidity events, lending, leverage, liquidations, hLP actions, parameter ramps,
and manipulation/transaction-ordering scenarios are outside the native ranking.

The controlled replay uses one $2.0M pool, 25% TVL/day modeled volume, one
fee/EMA/recenter configuration, and two flow regimes. The current-DH2 search
spans `peak_depth = 2…2,000` plus the separate CPMM control and
`fade_scale = 10⁻⁶…0.5`. Curve uses the pinned Twocrypto-NG v2.1-valid paper
domain `A = 0.1…1,000`, `gamma = 10⁻⁸…0.199`. Log-DH2 has no native protocol
domain, so its experimental `depth = 3…2,001` range is matched to Dusk's
observable total center-depth range and `half_width = 1…20,000 bps` spans a
declared compact-support search. Any iso point at a bound is domain-bounded rather than a
global family optimum. The iso solver scans the whole second-parameter log
domain and refines every sign-changing root; it does not assume monotonicity.

The invariant ranking index uses zero-fee exact-input orders from **1 to 2,000
bps of marked TVL** across both directions and price states from 0.67× to 1.50×
the center. Exact-output quotes remain safety/shape diagnostics rather than
ranking weights.

The native replay scores non-overlapping 12-hour historical windows after a
24-hour burn-in across calm, typical, and stress strata, four modeled flow
regimes, two seeds, and fixed/elastic routing. The benign, mixed, and
toxic-delayed regimes use 0.4 arrivals/minute; the low-volume/stuck regime uses
0.08 arrivals/minute so it tests sparse trade-driven observations as well as
lower total notional. Historical prices are observed; order arrivals,
information, outside routing, and arbitrage are modeled. The controller cannot
read the external reference price.

The Curve reference covers fixed-parameter swaps, dynamic fees, EMA,
profit-gated recentering, and price-scale updates. It excludes liquidity-event
hooks, parameter ramps, and the admin-fee claim operation that a liquidity
action may trigger and that is time-gated to at most once per 24 hours. It does
not assume an automatic daily claim transaction.
