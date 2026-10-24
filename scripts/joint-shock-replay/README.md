# Historical auction joint price, borrowing and hLP replay

This is a **historical auction benchmark**, not a replay of the current
emergency or flash liquidation instructions. Its insurance payouts and opening
reserve numbers must not be used as live-policy capital requirements. Current
liquidation covers at most the configured fraction of a *principal* shortfall
(75% by default) and cancels unpaid interest; the historical floor can draw
against all remaining debt, including interest.

Run from the repository root:

```sh
python3 scripts/joint-shock-replay/run.py
```

Requires Python 3, Cargo and the repository's cached Rust dependencies. The
script builds offline. It creates an isolated engine copy under
`target/joint-shock-results`, compiles with the `benchmark` feature, and appends
`stress_adapter.rs` to that copy only. Program files and scenario inputs are
hashed in `engine-manifest.json`. No deployed program code is replaced.

The adapter calls the benchmark-only atomic `settle_backstop_swap` transition,
applies existing hLP token-settlement helpers, and verifies both
reserve-custody identities. It exists because the stock native benchmark floor
wrapper rejects hLP token changes. The adapter assumes fee-free asset transfers;
it does not change pricing, risk, admission, debt, reserve or insurance math.

The 104 inputs are the unchanged October 10 SOL/USDC joint-shock set. Opening LP
equity is $1m; assumed SOL is $200; concentration is 5x; each hLP targets 2x.
Main borrowing is $75k USDC across ten loans at 65% initial collateral LTV, with
hLP entered before borrowing. Other load cases use the native admission and flow
limits and can add collateral to meet underwriting. The two rejected setup
configurations are retained as rejections, not successful stress paths.
Each position borrows once and makes no repayment before its floor, so the
recorded origination principal is its principal at liquidation.

The one-hour clock advances every 30 seconds. Arbitrage responds to the external
mark; risk does not receive an injected oracle. There are no outside auction
bidders; due floor liquidations execute sequentially before the next arbitrage
tick. The native 0.5% bounty is included. Withdrawals are attempted at second 60,
with a smaller valid fill if necessary. Main table rows omit withdrawals and
additional keeper delay. See `native/src/main.rs` for exact fee, clock, admission
and execution assumptions.

The runner executes 32 focused probes, 104 scenarios, the previous 24 funded,
capital and diagnostic controls, 66 fully funded loss paths, one severe-shock
policy contrast, and 132 capital boundary replays. It validates
preview/execution equality, native token custody, loss conservation and
event-level holder allocations. Historical auction insurance is computed from
the fully insured path, because injections change subsequent AMM execution,
and verified at the exact integer boundary and one atom less.

With event losses `L_i`, no insurance credits during the liquidation window,
and all floors within one 24-hour draw window, the current 50% daily limit requires:

```text
historical auction opening reserve = 2 * sum(L_i)
```

Results include raw JSONL traces, `scenario-summary.csv`, `main-results.csv`,
`insurance-requirements.csv`, `policy-contrast.csv`, `validation.json` and build
logs. The severe-shock contrast checks that a historical draw can exceed the
current 75%-of-principal-gap coverage ceiling. The native engine obtains that
ceiling from Dusk's current `LiquidationLossAllocation::quote_with_coverage`
function, and the runner independently checks the calculation. The contrast
CSV compares each funded auction floor with that ceiling **using the auction's
observed recovery**.
It is a policy bound, not a simulated live liquidation: the live sale, caller
reward, repayment and future AMM path can differ. A live-policy capital estimate
requires a separate replay through the current emergency or flash liquidation
path. The reference CSVs preserve the previous analysis candidate's numbers
for explicit comparison.
The current runtime reserves indexed funding interest at the start, whereas the
older candidate adjusted the endpoint after quoting; newer proportional-claim
rounding also differs. Small numerical changes are expected and reported rather
than overwritten.

This is a deterministic native economic replay. It does not replace the SVM
suite or estimate scenario probabilities, expected shortfall, premiums, repeated
shock capital, transfer-fee exposure, or a simultaneous two-sided borrowing book.
The same configuration changes under a different engine can change outcomes;
the source hashes identify the engine actually tested.
