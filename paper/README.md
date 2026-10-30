# Omnipair Dusk whitepapers

**2026-10-30 runtime update:** the retained research PDFs describe the earlier
time-incentive design. Current liquidation uses health alone, without registration;
see [the superseding policy](../docs/HEALTH_ONLY_LIQUIDATION.md).

**Start with [the hLP derivation and worked examples](HLP_DERIVATION.md).** It explains `xy=k`, `√p`, continuously maintained 2× leverage and the inventory-dependent hedge for concentration.

These research drafts were revised on October 23, 2026 for the integration candidate combining PR #45 at `04941f19` and PR #52 at `2f46e0e7`. The hLP accounting, harvest, cash-carry and flash-recovery mechanisms are described as implemented; final combined review remains pending. This is not a production deployment or audit certification.

## Read

- [Compact paper](dusk-v2-whitepaper.pdf): shared derivations and current mechanisms.
- [Expanded paper](dusk-v2-whitepaper-expanded.pdf): additional proofs, complete numerical trade/rebalance trace and source mapping.
- [Source map and validation boundary](SOURCES.md): code anchors, historical replay scope and remaining review.
- [Verification](verification.json): current PDF and arithmetic checks; [local CI receipt](precommit-verification.json).
- [Historical comparison](COMPARISON.md): unchanged July drafts and prior revisions.

Both papers explain flash liquidation, affordable partial-fill discounts, EMA-priced emergency execution with smoothed depth, stored margins/crowding, position actions, insurance, reusable governance delegation, and integrated hLP accounting/recovery. The retained native numerical replay predates the integrated hLP fee-marking change and is labeled as historical; current program behavior is covered separately by native and instruction tests.

## Build

Use Python 3 with ReportLab and Tectonic:

```sh
python3 paper/simulations/generate.py
python3 paper/simulations/verify_native.py
cd paper
tectonic --keep-logs dusk-v2-whitepaper.tex
tectonic --keep-logs dusk-v2-whitepaper-expanded.tex
```

`main.tex`, `appendices.tex`, `preamble.tex` and `references.bib` are the shared sources. Both top-level TeX files must be rebuilt after editing them. Render and visually inspect every page after compilation; update the source manifest and verification receipt when the implementation changes. Stable PDF filenames are retained beside the sources.

The generator writes disclosed mathematical illustrations, not a program emulator. The [native replay](simulations/README.md) separately checks selected program transitions, including concentration and both hLP vaults. Numerical examples do not establish profitability or liquidation liveness in every market.

This update changes documentation and reproducible paper evidence only; program and SDK interfaces are unchanged. The historical `previous/` bundle is unchanged.
