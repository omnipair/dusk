-- Generated implementation-status evidence for the Dusk versus V1 benchmark.
-- This is implementation evidence, not historical performance data.
WITH verification_commands(layer, command, result) AS (
  VALUES
    ('Benchmark workspace', 'CARGO_INCREMENTAL=0 cargo test --locked --offline --manifest-path analysis/v1-vs-dusk/Cargo.toml --no-fail-fast', '164 library + 14 data-quality passed; 0 failed'),
    ('All analysis targets', 'CARGO_INCREMENTAL=0 cargo check --locked --offline --manifest-path analysis/v1-vs-dusk/Cargo.toml --all-targets', 'passed'),
    ('Dusk native benchmark API', 'CARGO_INCREMENTAL=0 cargo test --locked --offline -p dusk --features benchmark benchmark_api::tests --no-fail-fast', '22 passed; 0 failed'),
    ('Default Dusk build', 'CARGO_INCREMENTAL=0 cargo check --locked --offline -p dusk', 'passed with benchmark feature disabled'),
    ('Dusk benchmark build', 'CARGO_INCREMENTAL=0 cargo check --locked --offline -p dusk --features benchmark', 'passed'),
    ('Schema catalog', 'PYTHONDONTWRITEBYTECODE=1 python3 analysis/v1-vs-dusk/schemas/validate.py --self-test', '16 artifact snapshots + 2 supporting schemas passed'),
    ('Notebook executor', 'PYTHONDONTWRITEBYTECODE=1 python3 analysis/v1-vs-dusk/scripts/execute_notebook.py --self-test', 'checksum, identity, duplicate-key, winner, exact-value, sandbox, and determinism passed'),
    ('Portable reports', 'cargo test ... caller_supplied_canonical_builder_accepts_all_three_artifacts -- --ignored', 'technical, governance, and public audiences passed Chromium verification')
)
SELECT * FROM verification_commands ORDER BY layer;
