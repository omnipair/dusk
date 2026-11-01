# Dusk finite-tail C1 vs CryptoSwap, with CPMM control

> **Historical finite-C1 decision snapshot; not a current-runtime validation.**
> The current nested tail-and-band implementation and fee/controller behavior
> are outside this report's measured system.

## Decision

**For Dusk’s intended long-tail markets, keep the finite-tail C1 invariant as the production candidate instead of adopting CryptoSwap’s global tail unchanged.** It preserves the same CryptoSwap center exactly, but stops releasing concentrated inventory after a finite transition and becomes constant-product. That materially reduces stale-price and toxic-tail loss while retaining very deep tiny-trade quotes.

This is a **prototype decision, not a ship decision**. CryptoSwap remains better for medium and large execution in many parameter regions, and the C1 candidate still needs Q48/u128, one-atom, SBF-CU, dynamic-fee, and controller validation.

### Why that decision

At the balanced profile and a 2× jump, CPMM loses **857.86 bps**, CryptoSwap loses **1351.80 bps**, and Dusk C1 loses **893.26 bps** of initial TVL. Dusk removes **92.8%** of CryptoSwap’s loss above CPMM. The aggressive profile removes **98.6%**.

Across the bounded service-constrained search, every selected Dusk profile still has lower 2× jump loss than the safest found CryptoSwap configuration whose balanced-start outward 10/100/1,000/2,000-bps quotes are all no worse. This is a result inside the tested parameter envelope—not a global or expected-profit dominance claim.

## The difference is not the center

For two assets, the current Dusk inner equation and the published CryptoSwap equation are algebraically identical under:

$$P_{\text{Dusk}}=2A_{\text{Curve}},\qquad s_{\text{Dusk}}=\gamma_{\text{Curve}}$$

Therefore `peak_depth` and `fade_scale` already span the same central family. No empirical parameter matching is needed to isolate the tail. The designs diverge only after Dusk begins its protocol-fixed finite transition.

Dusk and CryptoSwap overlap exactly for small trades that remain inside the common inner curve. Once a trade enters the finite convergence region, Dusk’s quote deteriorates toward CPMM sooner. That is the cost of refusing to expose LP inventory through CryptoSwap’s long global fade. In the balanced profile, the two designs both cost about 1.16 bps for a 100-bps-of-TVL trade, but Dusk costs 1,707 bps versus CryptoSwap’s 413 bps at a 1,000-bps trade.

## What the finite tail changes

Write imbalance as $v=|x-y|/(2\sqrt{xy})$ and product ratio as $q(v)=4xy/D^2$. Dusk follows CryptoSwap until $q_0=1-\beta s$, then uses a C1 bridge:

$$q(z)=q_0+m_0L\left(z-z^2+\frac{z^3}{3}\right),\qquad L=\frac{3(q_1-q_0)}{m_0},\qquad q_1=1-s$$

At the outer join, $q'=0$, so the level set becomes:

$$xy=\frac{D^2(1-s)}{4}$$

This is an **exact CPMM shape on a reduced product level**—not the original balanced CPMM product. CryptoSwap instead keeps fading globally and approaches its stronger asymptote only at extreme imbalance.

The finite tail stays close to CPMM during large stale-price moves. It does not erase the inventory already released in the shoulder, so Dusk remains lossier than CPMM: the gap is small for the balanced and aggressive profiles but material for the broad profile around a 2× move. The one-jump panel is fee-free immediate adverse selection; it is not a forecast and not a substitute for full path LVR. Reciprocal downside jumps were validated on a hold-value basis.

The fair conclusion is **specialization**, not universal dominance. At 100 bps, sampled Dusk configurations own the two-objective execution/loss frontier; at 1,000 bps, both families contribute frontier points. CryptoSwap offers stronger medium/large execution in many matched regions, while Dusk adds points that combine very deep micro-trade liquidity with near-CPMM tail protection—precisely the combination that matters for long-tail assets with thin external discovery and episodic toxic flow.

The bounded match is not hiding a better ordinary CryptoSwap configuration outside the coarse chart grid. In CryptoSwap’s mathematical edge limit, $s\to0$, $P\to\infty$, and $Ps^2$ remains finite; no finite parameter pair attains that envelope. Even there, Dusk’s 2× loss remains lower by **177.51 bps** (broad), **66.68 bps** (balanced), and **33.05 bps** (aggressive). This establishes a structural tail-safety difference under the four declared outward service constraints; it still does not establish expected LP profit, restoring service, or controller performance.

## Path behavior separates LVR from loss versus hold

A single jump’s loss versus the original inventory equals the arbitrage loss in that special case. Multi-step LVR is different: a 20-step trend to 2× creates much less immediate arbitrage profit than a one-block 2× jump, while a jump-and-reversal has positive cumulative LVR even though final loss versus hold returns to zero. Dusk C1 never exceeds CryptoSwap’s LVR in the tested paths. It is usually near or above CPMM and is 0.10 bp below CPMM in one aggressive chop case—a numerically tiny inversion rather than evidence of strict ordering. The broad profile remains exposed during chop; the aggressive profile is nearly CPMM-like outside its tiny center.

## The real drawback: restoring flow rebuilds concentration

C1 smoothing does not make restoring flow free. In an exact tail with $D=2$ and $q_t=1-s$:

$$x=\sqrt{q_t/p},\qquad y=\sqrt{q_tp}$$

Restoring to center makes Dusk output $y-1$, while a CPMM initialized with the same physical reserves outputs $y-q_t$. Dusk therefore withholds exactly:

$$1-q_t=s$$

That withheld output rebuilds the product level from $1-s$ to 1. It is why restoring quotes can be worse than a same-reserve CPMM even though outward quotes from balance stay between CPMM and CryptoSwap.

At a native 2× state, the Dusk restoring penalty versus the same-reserve CPMM is about 1.21% for the balanced profile and 0.12% for the aggressive profile, but 11.08% for the broad profile. This makes the broad $s=0.05$ profile a poor default for long-tail markets without stronger fee/recovery evidence. Restoring trades may have no divergence surcharge, but the invariant itself still performs this rebuild.

## Why the tail helps Dusk’s lazy recenter controller

When both the old and candidate centers leave the same physical reserves on the same exact-CPMM tail, balanced-equivalent protected Q is center-independent. All 41 such tested Dusk cases have zero protected-Q impairment. The first move that re-enters the convergence or concentrated region is not free and still needs retained funding. This metric measures the controller’s protected-Q budget only; it excludes subsequent arbitrage, fee-anchor changes, and timing.

## C1 solves the price jump, not the curvature kink

Both $q$ and $q'$ match at each join, so reserves and marginal price are continuous. But $q''$ is not matched: local depth changes abruptly at the inner-to-bridge join. In the balanced $\beta=0.5$ case, absolute log-price curvature moves from about 0.11 on the inner side to 4.82 on the bridge side. That is safer than the current C0 marginal-price gap, but it remains a router, rounding, and fixed-point test target.

Do not expose $\beta$ to operators. The 0.75 candidate creates the sharpest curvature for negligible economic gain and should be dropped. Keep 0.25 and 0.5 for the Rust prototype; 0.25 is smoother and wider, while 0.5 preserves more execution before converging. Lock one only after one-atom and CU profiling.

## Compute implication—not yet a compute result

A same-side Dusk tail swap, in either direction and for exact-in or exact-out, is a direct constant-product calculation. A tail-crossing or convergence-region swap still needs the general curve solve. CryptoSwap never gains a finite exact-tail branch. This is a credible architectural advantage, but the floating-point nested-iteration counts are implementation-specific and cannot predict Solana CU. Actual CU remains unmeasured.

## Mathematical and reproducibility gates

The analytical reference passes all 420 parameter cells, including off-center restoring, cross-center, exact-in/out, split, symmetry, join, and extreme-tail checks. The clean-room CryptoSwap root was independently reconciled with high-precision residuals.

What this report does **not** establish: fixed-point one-atom behavior, Solana CU, dynamic-fee profitability, retained-surcharge sufficiency, EMA/controller timing, hLP/lending interactions, historical-volume forecasts, or production readiness. Synthetic paths are deterministic stress cases, not probabilities.

## Final action plan

1. Build a standalone Q48/u128 Rust prototype of the finite-tail C1 curve; do not replace production math yet.
2. Differential-test every quote against this reference within one raw atom, especially both C1 joins and full restoring crossings.
3. Profile legacy SPL exact-in/out for center, convergence, same-tail outward/restoring, and tail-crossing paths. Keep the below-100k ordinary-swap target.
4. Replay Dusk’s uncapped divergence fee, volatility accumulator, retained funding, and lazy controller on historical long-tail paths.
5. Prototype the balanced and aggressive regions; do not use the broad $s=0.05$ profile as the default.
6. Lock the protocol bridge constant and production parameter bounds only after the fixed-point, CU, and controller gates pass.

## Methodology and source boundary

The CryptoSwap comparator is an independent implementation of the published two-asset equations. No Curve source code, constants, comments, tests, or implementation structure are copied or vendored. The pinned restrictive source license remains an explicit source-boundary warning. All benchmark data are local deterministic snapshots; no network access is required.

## Reproduce

From `analysis/dusk-c1-vs-cryptoswap`:

```bash
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 benchmark.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 -m unittest -v test_models.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 validate_results.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 execute_notebook.py
python3 build_report.py
```

The packaged HTML command and validation receipt are recorded in `README.md`.

## Durable artifacts

- `artifact.json`: canonical report source
- `report.html`: packaged portable report
- `analysis.ipynb`: executed notebook companion
- `results/validation.json`: independent reconciliation receipt
