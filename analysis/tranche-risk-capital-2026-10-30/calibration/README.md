# Junior-capital calibration and ledger prototype

Open [review.html](review.html) for the historical comparison and current configuration
notice. The [V1 controller specification](../v1-controller/README.md) supersedes this
study’s former 1%-minimum / 50%-maximum numerical recommendation. The fourteen-day
half-life is retained as a configurable default. [PREANALYSIS.md](PREANALYSIS.md) records the experiment assumptions;
[../PLAN.md](../PLAN.md) and [../exit-settlement/DECISIONS.md](../exit-settlement/DECISIONS.md)
contain the latest accepted product rules.

This study contains 1,514 deterministic simulation runs, including repeated
baseline configurations. They are not independent market observations. The exact
accounting and controller suites contain 31 tests. Neither the macro simulator
nor the rational-arithmetic ledger is a native Dusk implementation.

From this directory, use Python 3.9 or newer:

```sh
python3 -m unittest test_accounting.py test_model.py -v
python3 run.py
python3 refine.py
python3 report.py
python3 verify.py
```

The simulator and tests use the standard library. Rendering requires Matplotlib;
fontTools and Brotli are optional for rendering the brand font in charts. The
HTML embeds the supplied Omnipair logo and web font and opens without a server.

Saved evidence:

- [findings.json](results/findings.json): proposed settings and report metrics.
- [manifest.json](results/manifest.json): initial screen assumptions and grid.
- [refinement.json](results/refinement.json): follow-up profiles and run counts.
- [tests.log](results/tests.log): actual test output.
- [verification.json](results/verification.json): verification and content hashes.

The utilization-only comparison uses the same synthetic adaptive-rate framework,
capital budget and revenue rules as the combined controller. It is not a replay
of the existing Morpho-like Dusk implementation. Losses are exogenous, LP response
is assumed, and borrower behavior does not model interest-driven liquidations.
The model does not establish a safe APR cap for real collateral positions.

In the exact ledger, each call prepares or claims at most eight income records.
Historical storage and catch-up costs remain. The insolvency restriction clock is
tested; insolvency detection and resistance to repeated dust restrictions are not
implemented. An empty-senior reserve fallback is a placeholder awaiting a rule.
