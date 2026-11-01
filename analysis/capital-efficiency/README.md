# Dusk versus Omnipair v1: model and reproduction

**Complete report:** [Open the standalone report](report.html). It includes all
58 blocks, 11 charts, 5 tables, and the adaptive-concentration extension. The embedded
viewer failed to receive its payload; this file contains the complete payload
and reader and needs no external data connection. Browser verification passed
at 1440px and 390px, including the source menu. See `report_delivery.json`.

The initial sections are a **mathematical first pass** on local execution depth
and gross LVR. Later sections add historical-price replays with explicitly
modeled demand. Actual Dusk customer capture, toxic flow and optimal parameters
remain uncalibrated.

**Dynamic-fee extension:** [DYNAMIC_FEES.md](DYNAMIC_FEES.md) applies the current
volatility and gross-path distance formulas to conditional shock scenarios. It
also recalculates capital savings after traders pay those fees. Market-wide
effectiveness remains uncalibrated.

**Fixed-fee concentration sweep:** [CONCENTRATION_SWEEP.md](CONCENTRATION_SWEEP.md)
varies twelve Dusk concentrations with fee parameters and widths held constant.
It includes matched-execution capital and shock losses, synthetic one-hour paths
with customer routing, trading-only P&L, and order-sequence/routing sensitivity.
The results support a shortlist rather than a universally optimal concentration.

**Historical-price shortlist:** [HISTORICAL_SHORTLIST.md](HISTORICAL_SHORTLIST.md)
tests v1 and Dusk at 3×, 5× and 10× using five observed January 2025 SOL/USDT
hours, prior-day exchange clip sizes, and explicitly modeled customer arrivals
and execution budgets. It includes 1,015 replays, one-second prices, paired
demand seeds, arbitrage-timing sensitivity and matched-capital tests. Customer
demand remains a proxy, not observed Dusk flow.

**Adaptive concentration:** [ADAPTIVE_CONCENTRATION.md](ADAPTIVE_CONCENTRATION.md)
compares fixed curves with causal profitability, volume and volatility controllers
over continuous six-hour blocks. Training uses January 8–9 and later-date
evaluation uses January 15 and 19. The 1,440 primary replays favor fixed 10× on
mean trading profit. An additional 96 exploratory runs of a predeclared switch-cost
gate come within 2% of that profit, with lower sample trading drawdowns.
Reconfiguration preserves reserves and includes immediate arbitrage costs. Fast
governance authority and protected-depth admission are relaxed research assumptions;
the current program cannot execute this policy automatically.

## Reproduce

From the Dusk repository root, run:

```sh
python3 analysis/capital-efficiency/model.py
python3 analysis/capital-efficiency/dynamic_fees.py
python3 analysis/capital-efficiency/build_analysis.py
python3 analysis/capital-efficiency/concentration_sweep.py
python3 analysis/capital-efficiency/concentration_report.py
python3 analysis/capital-efficiency/fetch_market_data.py
python3 analysis/capital-efficiency/shortlist_replay.py --workers 4
python3 analysis/capital-efficiency/shortlist_report.py
python3 analysis/capital-efficiency/adaptive_replay.py --workers 4
python3 analysis/capital-efficiency/adaptive_diagnostics.py
python3 analysis/capital-efficiency/adaptive_report.py
```

Only the Python standard library is required. `model.py` writes six CSV scenario
tables, `results.json`, and `validation.json`. `build_analysis.py` also writes the
canonical report payload, source hashes, an independently executed SQLite metric
cross-check, and `analysis.ipynb` with captured sequential Python outputs.
The concentration commands calculate the fixed-fee sweep and extend the complete
report and notebook. They also write `concentration_results.json`, the
`concentration_*.csv` tables, `concentration_verification.sql`, and
`concentration_validation.json`. Run the report extension after `build_analysis.py`
because the original builder regenerates the earlier report sections.
Run `shortlist_report.py` after the concentration extension, and run
`adaptive_report.py` last to preserve the newest adaptive analysis. The adaptive
training selection is saved before evaluation; the cost-gated diagnostic is
explicitly exploratory because its evaluation followed the primary failure.
`fetch_market_data.py` needs network access on its first run; downloaded archives
are cached, checked against publisher SHA256 files, and excluded from Git.
Historical archive URLs and hashes are recorded in `market-data/manifest.json`.
`fee_optimizer.py` checks both sides of the outward-fee cap and is verified
against dense numerical searches before historical results are published.

To regenerate the standalone report after the analysis commands, use the installed
Data Analytics plugin root as the final argument:

```sh
node analysis/capital-efficiency/deliver_report.mjs "$DATA_PLUGIN_ROOT"
```

This uses the packaged renderer, chart extraction and browser verifier. A scoped
header-width correction avoids the bundled reader's horizontal overflow with
visible scrollbars. It preserves the canonical payload and shared reader. An
installed Chromium is required for browser verification; no browser is downloaded.

Open `analysis.ipynb` and change the `parameters` cell to explore capital,
volatility, widths, fixed fees and LP fee share. Its outputs update on execution;
the report narrative is the reviewed default scenario and does not automatically
update when notebook parameters change. Change the amplification and shock grids
inside `build_results` for further scenarios. The `Curve` class also accepts a
center price and supports off-center quotes.

## Default scenario

$1m initial ordinary reserve NAV, balanced at center price 1, 100% annualized
relative-price volatility, 365 days/year. Dusk core width is 100 bps and additional
shoulder width 400 bps. The execution example is a $10k quote-to-base buy with an
exogenous 30-bps input fee. All reserves are cash; active hLP and borrowing are
excluded. These inputs are illustrative, not observed or deployed settings.

| Configuration | All-in buy cost | Gross local LVR/day | Capital for v1's peak depth |
|---|---:|---:|---:|
| v1 / 1× | 230.09 bps | $342.47 | $1,000,000 |
| Dusk 2× | 138.53 bps | $684.93 | $500,000 |
| Dusk 5× | 70.09 bps | $1,712.33 | $200,000 |
| Dusk 10× | 50.09 bps | $3,424.66 | $100,000 |

LVR/day is an instantaneous local rate expressed per day. It is not a one-day
path forecast, an APR forecast, or realized arbitrage loss in a fee-paying pool.
Equal peak depth does not establish equal full-range depth or equal cash capacity.

## Derivation

Let price `P` be quote/base, `C` initial ordinary capital, `Pc` center, and
`L0 = C / (2 sqrt(Pc))` the matched v1 liquidity. For core width `wc` and outer
width `wo = wc + fade` (both decimal fractions), define:

```text
m = [1/sqrt(1+wc) + 1/sqrt(1+wo)] / 2    (nonzero shoulder)
Lpeak = A L0
Lconcentrated = (A-1) L0 / m
Ltail = A L0 - Lconcentrated
Lcore = Lshoulder = Lconcentrated / 2
```

With no shoulder, use only the core term in `m` and put all concentration in the
core. A positive tail requires `A < 1/(1-m)`, in addition to governance bounds.
For core/fade = 1000/4000 bps, the real-valued bound is approximately 8.694×,
so 10× is infeasible. Fixed-point rounding can slightly change boundary admission.

A range with liquidity `L` and square-root bounds `a,b` holds:

```text
s = clamp(sqrt(P), a, b)
x = L (1/s - 1/b)
y = L (s - a)
```

Add the tail inventory `(Ltail/sqrt(P), Ltail sqrt(P))`. Away from boundaries,
`dx/dP = -Lactive/(2 P^(3/2))`. The frictionless diffusion benchmark then gives:

```text
gross LVR rate = 0.5 sigma^2 P^2 |dx/dP|
              = Lactive sqrt(P) sigma^2 / 4
centered gross LVR / C = A sigma^2 / 8
```

For fixed initial liquidity, active-depth and dollar-LVR ratios versus v1 are
5, 2.970509 and 0.941017 across the 5× core, shoulder and tail, respectively.
Outside the center, current NAV differs between curves; `depth.csv` includes
current NAV explicitly. Actual path exposure requires the integral weighted by
realized variance and active depth, not just clock-time occupancy.

## Net toxic-flow comparison

At the matched centered point, define `r` as residual net toxic cost divided by
the configuration's gross LVR benchmark, after crediting toxic-trade fees to the
chosen LP owner. Then:

```text
(Dusk net toxic cost / capital) / (v1 net toxic cost / capital)
    approximately = A * r_Dusk / r_v1
```

Provided the v1 residual is positive, 5× requires `r_Dusk < r_v1/5` to reduce
toxic cost per capital. This is a condition, not a measured reduction. It also
does not decide overall profitability: organic fees and external lending yield
can offset higher toxic costs. If toxic fees are already inside `r`, do not add
them again as fee revenue.

For an isolated external upward jump to `Pext` and constant input fee `f`, the
arbitrage-optimal endpoint is `max(Pc, Pext*(1-f))`. Inventory differences give
net curve input and output. LP net shortfall equals the output marked at `Pext`
minus net curve input minus LP-owned fees; equivalently, arbitrage profit plus
non-LP fee allocation. This jump experiment is separate from diffusion LVR.

## Controls that need an engine replay

1. Curve shape and finite-size all-in execution quality.
2. Dynamic fees, fee ownership, organic-demand response and adverse selection.
3. Sticky-center movement, deferred moves and protected recenter funding.
4. External prices, jumps, mean reversion, informed flow and arbitrage latency.
5. Cash utilization, borrowing, active hLP, withdrawals and rejected demand.

Current Dusk defaults to 1× with dynamic coefficients disabled. Its volatility
accumulator uses previous successful swaps' capped absolute relative moves,
not statistical variance. Retained dynamic surcharge can be outside yLP NAV.
Do not count synthetic hLP funding as extra trader-visible liquidity or internal
funding interest as incremental consolidated-system revenue.

Replay the same external opportunities, allowing trades to differ in size and
execution. Compare ordinary yLP, both hLP sides and consolidated participants.
Measure signed markouts at separate horizons; do not subtract both markout-based
loss and LVR from the same P&L.

## Evidence and validation

- Dusk curve and accounting: [CONCENTRATION.md](../../programs/dusk/CONCENTRATION.md),
  [curve.rs](../../programs/dusk/src/transitions/amm/curve.rs),
  [fees.rs](../../programs/dusk/src/transitions/amm/fees.rs),
  [AMM transitions](../../programs/dusk/src/transitions/amm/mod.rs), and
  [configuration](../../programs/dusk/src/state/market.rs).
- v1 source baseline: `omnipair-rs` local commit
  `84a8ef7a2a3a81e750435b86d7191bf43413d6a4`; Dusk HEAD
  `26bc75b217a96a6241b82b05cd52c1b587272dc1` plus existing local changes.
  Exact inspected file hashes are in `source-notes.json`.
- Theory: [Automated Market Making and Loss-Versus-Rebalancing](https://arxiv.org/html/2208.06046v5)
  and [Arbitrage Profits in the Presence of Fees](https://arxiv.org/abs/2305.14604).

Checks passed for centered NAV, finite-difference gamma, equal-depth LVR,
reciprocal symmetry, boundary continuity, fee conservation, CPMM swap and jump
closed forms, infeasible shape rejection, and independent SQLite recomputation
of the headline metrics. All seven notebook code cells executed sequentially in
Python. Jupyter/nbformat dependencies were unavailable, so kernel-level execution
and independent notebook schema validation were not run.

The report payload passed the analytics validator, but the native rendering
tool's success response did not establish delivery: the user's screenshot showed
`missing_artifact_payload`. Native delivery failed. The complete standalone report
subsequently passed the packaged browser verifier, including exact payload
equality, all block counts, element visibility, desktop/mobile overflow checks,
source interaction, and absence of external requests or browser errors.

No on-chain engine replay, historical return analysis, integer quote-parity test,
or dynamic-fee calibration is claimed. The analysis adds files only; it does not
change protocol behavior or choose a deployment configuration.
