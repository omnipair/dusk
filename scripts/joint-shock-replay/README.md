# Historical auction joint price, borrowing and hLP replay

This host-native tool exercises historical auction helpers against the current
source. It does not execute the deployed emergency or flash instructions, and
its senior/hLP allocations do not validate the complete junior-to-senior
waterfall. Use the native tranche tests and LiteSVM suite for that contract.
The standalone coverage fund and funded/capital scenarios have been removed.
See [the current credit-loss policy](../../docs/CREDIT_LOSS_WATERFALL.md).

```sh
python3 scripts/joint-shock-replay/run.py
```

Requires Python 3, Cargo, and cached Rust dependencies. Builds offline into
`target/joint-shock-build`, with an isolated engine copy and results under
`target/joint-shock-results`. It adds `stress_adapter.rs` only to that copy and
records source and scenario hashes in `engine-manifest.json`.

The adapter uses benchmark-only `settle_backstop_swap`, existing hLP settlement
helpers, fee-free asset transfers, and reserve-custody checks. It does not
substitute pricing, admission, debt, or reserve math.

The 104 October 10 SOL/USDC scenarios start with $1m LP equity, SOL at $200,
5x concentration, and 2x hLP targets. Main borrowing is $75k USDC across ten
loans. Positions borrow once, so origination principal equals remaining
principal before liquidation. The clock advances every 30 seconds for an hour;
external marks drive arbitrage rather than injected oracle data. Floors execute
sequentially without outside bidders, including the fixed caller bounty.
Withdrawals are attempted at second 60 with a smaller valid fill if necessary.

The runner executes 32 custody probes, the scenario set, and diagnostic controls.
It checks preview/execution equality, reserve custody, nominal debt-shortfall conservation,
and event-level holder allocations. Results include JSONL traces,
`scenario-summary.csv`, `main-results.csv`, `validation.json`, and build logs.
The historical floor receipt reports unrecovered nominal debt, including canceled
interest. Its holder attribution is not the current funded-principal loss waterfall.
The retained driver does not perform income-epoch archival maintenance. Runs
against the current income books can stop at that boundary and are recorded as
incomplete; they must not be counted as successful economic paths. This is a
historical diagnostic, not a current liquidation validation gate.
Incomplete or rejected paths are recorded explicitly; counts are computed from
this run rather than assumed from an older engine.

`analysis-reference.csv` and `analysis-capital-reference.csv` are historical raw
evidence from the removed coverage-fund design. They are not used by this runner
and do not describe current funding requirements. This replay does not estimate
scenario probabilities, repeated shocks, transfer-fee exposure, or simultaneous
two-sided borrowing. Source hashes identify the actual engine tested.
