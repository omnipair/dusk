# Dusk research archive

Research on concentration, execution, LP losses, and borrowing risk is collected
here with its models, notebooks, figures, and retained experiment results.

| Study | Start here | Scope |
| --- | --- | --- |
| Capital efficiency and adaptive concentration | [Report](capital-efficiency/report.html), [reproduction guide](capital-efficiency/README.md) | Fixed concentration sweeps, dynamic fees, historical-price replays, and concentration switching costs. |
| CryptoSwap versus DH2 | [Decision report](cryptoswap-vs-dh2/DECISION_REPORT.md), [interactive report](cryptoswap-vs-dh2/report.html) | Historical invariant and controller comparisons, with CPMM controls. |
| Finite-tail C1 versus CryptoSwap | [Decision report](dusk-c1-vs-cryptoswap/DECISION_REPORT.md), [interactive report](dusk-c1-vs-cryptoswap/report.html) | Historical finite-tail prototype, execution depth, and jump-loss comparisons. |
| Borrowing risk pricing | [Report](risk-pricing/report.html), [notebook](risk-pricing/dusk_risk_pricing.ipynb) | Pair-wide and position-level risk premiums under modeled liquidation stress. |
| Dusk versus v1 | [Runner and methodology](v1-vs-dusk/README.md), [evidence report](v1-vs-dusk/results/final-report/report.html) | Historical counterfactual benchmark tooling and retained evidence. The evidence report identifies where the comparative Dusk score remains unavailable. |

## Reading the results

The DH2 and C1 collections describe earlier experimental designs. Their
[DH2 legacy notice](cryptoswap-vs-dh2/LEGACY_NOTICE.md) and
[C1 source boundary](dusk-c1-vs-cryptoswap/SOURCE_BOUNDARY.md) define the models
that were tested. Read conclusions within those boundaries when assessing the
current nested tail-and-band implementation.

The concentration study uses historical prices with modeled customer demand.
Its [adaptive concentration analysis](capital-efficiency/ADAPTIVE_CONCENTRATION.md)
explains the governance and integration assumptions behind fast curve changes.
The risk-pricing scenarios are conditional model outputs rather than selected
runtime interest rates.

Each collection keeps its own reproduction instructions. Larger historical data
exports and external services referenced by the benchmark must be obtained using
its source and coverage requirements; retained receipts do not replace those
inputs. Set `DUSK_REPO_ROOT` to the checkout root where a report uses that path
placeholder. `OMNIPAIR_V1_REPO_ROOT` identifies the separate v1 checkout in
source provenance; `DATA_PLUGIN_ROOT` is needed only by the optional local
report-delivery helper.

## Provenance

The CryptoSwap, C1, risk-pricing, and v1 benchmark collections were recovered from
local branch `feat/analysis-integration-followup`, commit
`ad99bbe0464182a5d6d1de2bffcc230957ecd066`. The capital-efficiency collection was
recovered from the local research files on 1 November 2026. Laptop-specific paths
in its provenance metadata have been replaced with portable path placeholders.
CSV line endings have been normalized to LF; recorded values are unchanged.

Research concerning liquidation margins and parameter calibration is already
maintained in [docs/calibration](../docs/calibration/README.md).
