# Joint price, borrowing and hLP replay

Run from the repository root:

```sh
python3 scripts/joint-shock-replay/run.py
```

Requires Python 3, Cargo and the repository's cached Rust dependencies. The
script builds offline. It creates an isolated engine copy under
`target/joint-shock-results`, compiles with the `benchmark` feature, and appends
`stress_adapter.rs` to that copy only. Program files and scenario inputs are
hashed in `engine-manifest.json`. No deployed program code is replaced.

The adapter calls the same atomic `settle_backstop_swap` transition as the
instruction, applies existing hLP token-settlement helpers, and verifies both
reserve-custody identities. It exists because the stock native benchmark floor
wrapper rejects hLP token changes. The adapter assumes fee-free asset transfers;
it does not change pricing, risk, admission, debt, reserve or insurance math.

The 104 inputs are the unchanged October 10 SOL/USDC joint-shock set. Opening LP
equity is $1m; assumed SOL is $200; concentration is 5x; each hLP targets 2x.
Main borrowing is $75k USDC across ten loans at 65% initial collateral LTV, with
hLP entered before borrowing. Other load cases use the native admission and flow
limits and can add collateral to meet underwriting. The two rejected setup
configurations are retained as rejections, not successful stress paths.

The one-hour clock advances every 30 seconds. Arbitrage responds to the external
mark; risk does not receive an injected oracle. There are no outside auction
bidders; due floor liquidations execute sequentially before the next arbitrage
tick. The native 0.5% bounty is included. Withdrawals are attempted at second 60,
with a smaller valid fill if necessary. Main table rows omit withdrawals and
additional keeper delay. See `native/src/main.rs` for exact fee, clock, admission
and execution assumptions.

The runner executes 32 focused probes, 104 scenarios, the previous 24 funded,
capital and diagnostic controls, 66 fully funded loss paths, and 132 capital
boundary replays. It validates preview/execution equality, native token custody,
loss conservation and event-level holder allocations. Required insurance is
computed from the fully insured path, because injections change subsequent AMM
execution, and verified at the exact integer boundary and one atom less.

With event losses `L_i`, no insurance credits during the liquidation window and
prior losses `C_i`, the current 20% event and 50% daily limits require:

```text
opening reserve = max(2 * sum(L_i), max(C_i + 5 * L_i))
```

Results include raw JSONL traces, `scenario-summary.csv`, `main-results.csv`,
`insurance-requirements.csv`, `validation.json` and build logs. The reference
CSVs preserve the previous analysis candidate's numbers for explicit comparison.
The current runtime reserves indexed funding interest at the start, whereas the
older candidate adjusted the endpoint after quoting; newer proportional-claim
rounding also differs. Small numerical changes are expected and reported rather
than overwritten.

This is a deterministic native economic replay. It does not replace the SVM
suite or estimate scenario probabilities, expected shortfall, premiums, repeated
shock capital, transfer-fee exposure, or a simultaneous two-sided borrowing book.
The same configuration changes under a different engine can change outcomes;
the source hashes identify the engine actually tested.
