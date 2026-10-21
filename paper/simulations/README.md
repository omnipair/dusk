# Reproduce the whitepaper evidence

There are two separate evidence sources:

1. `generate.py` checks mathematical identities and writes tables, CSVs and vector figures. It requires Python 3 and ReportLab.
2. `native_hlp_flow.rs` runs the Dusk native benchmark API. `native-hlp-flow.json` retains its eight cases plus derived holder valuations and fee breakdowns. `verify_native.py` checks conservation and the paper's numbers in that retained evidence; it does not re-execute Rust or test token CPIs.

## Check the paper artifacts

```sh
python3 paper/simulations/generate.py
python3 paper/simulations/verify_native.py
```

## Re-execute the native trace

Use Dusk source commit `5c0aca7fcb582418f1c5b99061d4ff7605b6f185`, not the unmerged accounting candidate. Create a temporary directory containing that commit's `git archive` under `source/`, and a separate Rust binary crate under `runner/`. Copy `native_hlp_flow.rs` to `runner/src/main.rs`. Its manifest is:

```toml
[package]
name = "hlp-flow-analysis"
version = "0.1.0"
edition = "2021"

[dependencies]
dusk = { path = "../source/programs/dusk", features = ["benchmark"] }
anchor-lang = "0.31.1"
serde_json = "1"
```

Run `cargo run --manifest-path runner/Cargo.toml` and save stdout as JSON. A shared Cargo target directory can avoid duplicating build artifacts. The runner checks native preview/execution equality, market invariants, and cash/trader/fee conservation before emitting cases. Keep its lockfile if repeating in a different toolchain environment.

The raw runner emits a case array. The retained JSON wraps it with source/assumption metadata and adds these derived fields:

- `fee_atoms`: integers parsed from the native fee receipt.
- Holder baseline: ordinary `100*p+100`, META hLP `50*p`, quote hLP `50`, with zero hLP baseline in no-hLP cases.
- Claimable fees: LP fee custody allocated by pre-trade yLP share fraction; the protocol share remains separate. Sub-atom holder rounding is ignored in this displayed comparison.
- Net value including claimable fees, difference from holding, and aggregate conservation error.

All runs use nine token decimals, no elapsed funding, no transfer fees, no dynamic surcharge, fixed geometry during the trade and final marginal pool price as the comparison mark. Parameters include amplification 1/4, both hLPs present/absent for zero-fee controls, and 30 bps swap fees with 0/100% LP-fee compounding. Each swap is followed by native owned-share settlement for both vaults. This is not a validator test, an external arbitrage simulation or a prediction of returns.

The full CI receipt is separate in `../precommit-verification.json`. Its instruction-level tests provide additional evidence; they are not the source of the numeric tables here.
