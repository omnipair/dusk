# Standalone finite-C1 Q48 reference

This dependency-free crate is the integer contract for evaluating the two
protocol-fixed bridge candidates before production integration:

- `BridgeStart::Quarter`: transition begins at `delta = fade_scale / 4`;
- `BridgeStart::Half`: transition begins at `delta = fade_scale / 2`.

It uses raw `u64` reserves, Q48 dimensionless values, and `u128` intermediate
arithmetic only. There is no big-integer dependency. Same-side tail swaps use
an exact integer CPMM fast path; every crossing uses the full finite-C1 curve.

## Verify

Run from the parent `analysis/dusk-c1-vs-cryptoswap` directory:

```bash
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 generate_q48_fixtures.py
cargo test --manifest-path rust-q48/Cargo.toml --all-targets
cargo run --quiet --manifest-path rust-q48/Cargo.toml --example iteration_report
```

The test contract covers:

- value and discrete-slope continuity at both C1 joins;
- all four directional region crossings;
- outward and restoring exact-tail swaps in both reserve orientations;
- exact-input maximal output and exact-output minimal input by one raw atom;
- reserve symmetry, split resistance, and no-profitable-round-trip rounding;
- constructor bounds, raw-reserve extremes, and solver iteration ceilings;
- 576 exact-input plus 576 exact-output analytical differential fixtures.

`SolverIterations` separates Q evaluations, inner-Q iterations, invariant
iterations, and reserve iterations. The `iteration_report` example runs the
same 144-quote matrix for each bridge candidate, making the native arithmetic
burden explicit. These counts are not Solana SBF compute units.
