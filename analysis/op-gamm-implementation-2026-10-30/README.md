# Bounded OP-GAMM numerical evidence

30 October 2026. The kernel is integrated into Dusk. [Implementation status](../../docs/OP_GAMM_IMPLEMENTATION_STATUS.md) distinguishes current checks from the older feasibility experiments in this directory.

## Current mathematical contract

For native marginal price N, selected reference O, weight λ, and adjustment cap M:

```
P = N * exp(clip(λ * ln(O/N), -ln(M), ln(M)))
```

The positive-weight implementation accepts 1–9,999 basis points; λ = 0 dispatches to native Dusk without a feed. Clipping preserves a nonzero full-range native tail. It does not eliminate losses from a wrong oracle or force exact tracking outside the cap.

Let u be normalized native sqrt-price, t = ln(u), with native band X = A/u+B and Y = A*u+D. The conditional invariant is:

```
U = ln(x) - ln(X(t)) + integral correction(t) dt
correction(t) = A*(u - p/u)/(Y + p*X), p = P/C
```

The native primitive is analytic. The remaining integral splits at concentration and clipping boundaries. CPMM, clipped and half-weight regions use analytic paths; other cells use an outward-rounded Taylor quotient with an explicit remainder. The active arithmetic is **Q56**, with an absolute integral target of **1e-9**, at most 128 cells and subdivision depth 24. Endpoint arithmetic and boundary uncertainty are included in the returned interval. Nearby endpoints can use a monotone-price enclosure of the invariant gradient; that enclosure must meet the same width target.

The output solver uses at most 12 candidate iterations. A numerical root or midpoint is never sufficient authorization: settlement requires a nonnegative lower invariant bound for the rounded ownership endpoint, including fee/carry changes. Excessive uncertainty or work fails the transaction. Value-only closeout calculations use the same certified output while omitting unused settlement geometry.

No floating-point value authorizes a trade. The independent reference CSVs retain their original Q64 integer scale; Rust explicitly converts those references to Q56 before comparison. They are finite numerical evidence, not a proof of every possible input.

## Historical files

The initial `summary.json`, `compute-probe.json`, extended certificate results, unlimited diagnostic profiles and Q48 counterfactual describe earlier implementations. The status fields in `summary.json` are the archived pre-integration checkpoint, not the current implementation status. They must not be cited as current complete-route performance. Unlimited runs never establish Solana feasibility. Current route tests live in `tests/v2-final-smoke.test.ts`; the probe supports `OP_GAMM_SOLVER=1` for isolated output-solving measurements.

The [initial integration validation record](validation.json) binds the 30 October checks to their source and artifact hashes. The [31 October activation-policy validation](validation-oracle-activation-2026-10-31.json) records the full CI rerun after removing creation-time oracle restrictions, including repeated governed activation/deactivation with outstanding leverage and both hLP vaults. The [31 October atomic-creation validation](validation-oracle-creation-2026-10-31.json) covers selecting the initial policy in `initializeMarket`, generated-client address resolution, creation rollback, first-liquidity feed checks and the full existing regression suite. The pricing kernel is unchanged; the isolated solver measurements below were not rerun for these policy changes.

## Current solver measurement

`solver-extended-compute-probe.json` records the integrated Q56 output solver under the normal 1,400,000-CU transaction limit: **144/144 cases completed**, with **765,487 CU** maximum. This is the isolated solver, not the total cost of a Dusk instruction or composed transaction. Full route coverage remains in the LiteSVM suite.

## Independent reference

`generate-reference.py` uses mpmath at 80 decimal digits, direct inventory construction, ratio inversion by bisection, and independent quadrature. It generates the committed CSV fixtures; Rust does not call Python at runtime. The grid covers four valid concentration profiles, four positive weights, three references, and three endpoint moves, including both directions across bands. The reference is numerical evidence, not a formal proof of all possible inputs.

## Reproduce

From the repository root, with its ordinary JavaScript dependencies installed:

```sh
python3 -m venv /tmp/dusk-op-gamm-validation
/tmp/dusk-op-gamm-validation/bin/python -m pip install mpmath==1.3.0
/tmp/dusk-op-gamm-validation/bin/python analysis/op-gamm-implementation-2026-10-30/generate-reference.py
cargo test -p dusk math::op_gamm
cargo build-sbf --manifest-path tests/fixtures/op_gamm_compute/Cargo.toml --sbf-out-dir target/deploy
node analysis/op-gamm-implementation-2026-10-30/compute-probe.mjs
OP_GAMM_EXTENDED=1 node analysis/op-gamm-implementation-2026-10-30/compute-probe.mjs
OP_GAMM_EXTENDED=1 OP_GAMM_SOLVER=1 node analysis/op-gamm-implementation-2026-10-30/compute-probe.mjs
OP_GAMM_EXTENDED=1 OP_GAMM_DIAGNOSTIC_UNLIMITED=1 node analysis/op-gamm-implementation-2026-10-30/compute-probe.mjs
```

Random ephemeral test public keys change logs but not economic inputs or the compute conclusions. They are local fixture identities, not deployment accounts. For full Dusk transaction coverage, oracle authentication, hLP accounting and governance tests, see [implementation status](../../docs/OP_GAMM_IMPLEMENTATION_STATUS.md).

## Archived Q48 counterfactual

`q48-experiment.patch`, `q48-extended-*` and `q48-tests.txt` record an earlier experiment. It fit 98/144 isolated cases and was not selected. The active implementation is Q56 with later mathematical and runtime optimizations. Do not apply the archived patch to the integrated implementation.
