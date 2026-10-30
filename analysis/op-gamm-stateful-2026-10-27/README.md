# Stateful OP-GAMM research and two isolated accounting repros

Pinned Dusk: `77877d576018fd2bd8191d13f05534b0484e2282` (#45 baseline used in the preceding analysis). These dated results were generated without production source edits. This is a host-native accounting harness and a floating-point oracle model, not an SVM/instruction certification.

Run from the Dusk repository with Python 3.12+ and NumPy:

```sh
python3 -m venv /tmp/dusk-op-gamm-venv
/tmp/dusk-op-gamm-venv/bin/pip install -r analysis/op-gamm-stateful-2026-10-27/requirements.txt
/tmp/dusk-op-gamm-venv/bin/python analysis/op-gamm-stateful-2026-10-27/reproduce.py
```

That rebuilds the archived source using locally cached Cargo dependencies and runs the two minimal repros, each before and after its candidate fix. Add `--no-build` to reuse the binary. Add `--all` for the full state/decimal/attack/recovery matrix. The working repository's `programs/`, SDK and git state are never modified. The snapshot is regenerated from the pinned commit, including any experimental changes it already contained; it does not use the dirty working tree.

- `minimal_repros.py`: native one-vault-frozen round trip and shadow carried-interest round trip.
- `apply_research_fixes.py`: switchable candidate arithmetic changes in the archived source, default off. It creates no feature gate or release decision for Dusk.
- `experiment.py`: exact integer endpoint port, native JSON client, oracle conditional invariant, materialized-endpoint certificate and carried-value correction.
- `run_cycles.py`, `test_candidate_fixes.py`, `regression_checks.py`, `validate_extra.py`: stress, rounding, native-zero-limit and repeated-cycle tests.
- `results/`: inputs, per-trade results, minimal before/after state snapshots and validation evidence.

See [the report](../../docs/OP_GAMM_FROZEN_CARRY_REPRO.md) for the findings, formulas, economic assumptions and remaining limits. Results from `fault_recovery_cycles_fixed.csv` apply both candidate fixes; unsuffixed files retain the original accounting controls. The native `carried` control uses Dusk's normal pay-or-carry decision; forced carry is an explicit shadow-model stress, not a claim that ordinary native trades always carry funding.

All prices and market movements in this bundle are controlled synthetic stresses. No historical feed error probabilities, borrower demand or deployable parameter calibration are inferred.

Publication note: local absolute paths in captured diagnostics were made repository-relative. The publication manifest includes the archived result bytes and portable source. Python 3.12+ and the pinned Rust commit must be available; use a full Git clone. The build uses the local Cargo cache (`--offline`).
