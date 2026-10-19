# Omnipair Dusk whitepapers

**Start with [the hLP derivation and worked examples](HLP_DERIVATION.md).** It explains `xy=k`, `√p`, continuously maintained 2× leverage and the inventory-dependent hedge for concentration.

These research drafts were revised on October 19, 2026 against PR #45 at `5c0aca7fcb582418f1c5b99061d4ff7605b6f185`. Incoming #52 and agreed unfinished accounting work are explicitly identified. This is an implementation snapshot, not a production deployment or audit certification.

## Read

- [Compact paper](dusk-v2-whitepaper.pdf): shared derivations and current mechanisms.
- [Expanded paper](dusk-v2-whitepaper-expanded.pdf): additional proofs, complete numerical trade/rebalance trace and source mapping.
- [Source map and pending-work boundary](SOURCES.md): code anchors and what #52 still needs to implement.
- [Verification](verification.json): current PDF and arithmetic checks; [local CI receipt](precommit-verification.json).
- [Historical comparison](COMPARISON.md): unchanged July drafts and prior revisions.

Both papers explain flash liquidation, affordable partial-fill discounts, EMA-priced emergency execution with smoothed depth, stored margins/crowding, position actions, insurance and reusable governance delegation. The hLP accounting/harvest/flash-recovery changes still under development are labeled as pending, rather than described as current runtime behavior.

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
