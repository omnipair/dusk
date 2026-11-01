# Dusk finite-tail C1 vs CryptoSwap

> **Historical finite-C1 research snapshot.** This package informed the curve
> design but does not validate or rank the current nested tail-and-band Dusk
> implementation. Use `programs/dusk/CONCENTRATION.md` and commit-bound current
> tests for production claims.

Focused, deterministic decision package for Dusk's long-tail concentrated AMM.
CPMM is retained as the control.

## Decision

Advance the finite-tail C1 invariant to a standalone Q48/u128 Rust prototype
for Dusk long-tail markets. Do not replace production math until fixed-point,
one-atom, dynamic-fee/controller, and SBF compute gates pass.

This is not a universal CryptoSwap-versus-Dusk winner claim:

- both models have the exact same inner curve when
  `peak_depth = 2 * Curve A` and `fade_scale = Curve gamma`;
- CryptoSwap preserves stronger medium/large execution through its global
  fade;
- Dusk C1 stops at a finite exact-CPMM-shaped tail, reducing toxic-tail loss
  and protected-Q recenter impairment;
- restoring flow must rebuild the reduced product level and can receive less
  output than a CPMM initialized with the same physical reserves;
- C1 makes marginal price continuous but leaves a curvature kink.

The full reasoning and numerical evidence are in:

- `DECISION_REPORT.md`
- `report.html`
- `artifact.json`
- `analysis.ipynb`

## Reproduce from scratch

Run from this directory with no network access:

```bash
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 benchmark.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 -m unittest -v test_models.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 validate_results.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 execute_notebook.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 build_report.py
```

Regenerate and verify the standalone Q48/u128 integer reference:

```bash
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 generate_q48_fixtures.py
cargo test --manifest-path rust-q48/Cargo.toml --all-targets
cargo run --quiet --manifest-path rust-q48/Cargo.toml --example iteration_report
```

The generator commits 576 exact-input and 576 exact-output analytical rows.
Both Rust differential suites accept at most one raw atom of disagreement.
The deterministic iteration report currently records:

| Fixed bridge start | Quotes | Q evaluations | Inner-Q iterations | Invariant iterations | Reserve iterations | Total solver iterations | Exact-tail quotes |
|---|---:|---:|---:|---:|---:|---:|---:|
| `fade_scale / 4` | 144 | 6,121 | 131,760 | 2,244 | 5,617 | 139,621 | 36 |
| `fade_scale / 2` | 144 | 6,122 | 134,544 | 2,256 | 5,618 | 142,418 | 36 |

These are native reference-operation counts, not SBF compute units. They make
the two protocol-fixed bridge candidates directly comparable without exposing
another operator parameter. Exact same-side tail quotes report zero nonlinear
solver iterations because they use the integer constant-product fast path.

Package and verify the portable report:

```bash
$NODE_BIN \
  $REPORT_BUILDER \
  --input artifact.json \
  --output report.html \
  --timeout-ms 30000 \
  --ready-timeout-ms 10000
```

The final portable-report receipt passed:

- canonical artifact validation;
- chart extraction;
- source dialog and semantic interaction;
- responsive checks at 1,440 px and 390 px;
- 35 blocks, 6 charts, and 6 tables.

`analysis.ipynb` was executed with the included deterministic stdlib runner
because this workspace does not provide `nbformat`, `nbclient`, IPython, or a
Jupyter executable. All seven code cells passed; the notebook records this
fallback in its metadata.

## Package contents

- `models.py` — independent analytical CPMM, CryptoSwap, and Dusk C1 models.
- `benchmark.py` — deterministic sweeps and result generation.
- `test_models.py` — high-precision residual, geometry, quote, restoring, and
  solver regressions.
- `validate_results.py` — independent row-count, source-hash, reciprocal,
  path-LVR, tail, recenter, and service reconciliation.
- `analysis.ipynb` / `execute_notebook.py` — executed analytical companion.
- `build_report.py` — canonical report and Markdown builder.
- `SOURCE_BOUNDARY.md` — clean-room and license boundary.
- `generate_q48_fixtures.py` — deterministic exact-in/out analytical fixtures.
- `rust-q48/` — dependency-free Q48/u128 integer model, one-atom differential
  checks, boundary/safety regressions, and bridge-iteration report.
- `results/` — exact CSV/JSON snapshots used by the report.

## Scope and limits

Established by this package:

- exact inner-curve identity;
- C1 value and marginal-price continuity;
- exact CPMM shape on `xy = D²(1-s)/4` in the tail;
- balanced outward, restoring, cross-center, exact-in/out, split, and
  off-center numerical behavior;
- reciprocal one-jump stresses, deterministic path LVR, protected-Q recenter
  impairment, and a 200-cell service/risk sweep;
- 420/420 analytical safety cells passed with zero solver failures.
- the standalone Q48/u128 reference matches 1,152 analytical exact-in/out
  fixtures within one raw atom and passes deterministic join, crossing,
  restoring, tail, rounding, split, round-trip, and extreme-state tests.

Not established here:

- production/on-chain Q48/u128 equivalence (the standalone reference is now
  verified, but has not yet been wired into the Dusk program);
- Solana SBF compute units;
- dynamic-fee revenue or retained-surcharge sufficiency;
- EMA and lazy-controller timing;
- hLP, lending, leverage, and liquidation interaction;
- expected live order flow or historical long-tail profitability.

No Curve source code, constants, comments, tests, or implementation structure
are copied or vendored. The comparator is independently derived from published
mathematics; the restrictive upstream source license remains explicit in
`SOURCE_BOUNDARY.md`.
