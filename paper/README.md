# Omnipair Dusk whitepapers

Two research drafts revised against Dusk commit `263c1635ad87ee36ffd9bf0b970457e4be965326` on September 10, 2026. Dusk is under active development and has no production deployment.

- `dusk-v2-whitepaper.tex` / `.pdf`: compact paper (10 pages).
- `dusk-v2-whitepaper-expanded.tex` / `.pdf`: expanded paper (26 pages) with derivations, worked examples, and implementation mapping.
- `main.tex`: shared mechanism text; expanded-only passages use `\ifexpanded`.
- `appendices.tex`: expanded derivations and review evidence.
- `preamble.tex`, `references.bib`: shared formatting and references.
- `SOURCES.md`: claim-to-code mapping, revision scope, and verification notes.
- `source-manifest.json`: hashes of the implementation files named in the source map.
- `simulations/generate.py`: reproducible mathematical illustrations, tables, and vector figures.

The original July drafts were recovered from the earlier whitepaper drafting task because the `paper/` directory was absent from this checkout. This revision replaces their obsolete mechanism descriptions rather than retaining a historical warning over unchanged content. The old claims of synthetic hLP trader depth, fixed two-times leverage on every curve, iterative pre-rebalancing, universal non-compounding yield, and close-factor public liquidation are superseded.

## Build

Use Python 3 with ReportLab for vector charts, then Tectonic for LaTeX and bibliography compilation:

```sh
python3 paper/simulations/generate.py
cd paper
tectonic --keep-logs dusk-v2-whitepaper.tex
tectonic --keep-logs dusk-v2-whitepaper-expanded.tex
```

The bundled Codex Python runtime includes ReportLab. A conventional LaTeX installation can compile either source using `latexmk -pdf` instead. Generated figures are PDF vectors and require no SVG converter. The final PDFs are kept beside their stable LaTeX entry points, matching the original paper bundle.

## Illustrations and scope

The generator discloses assumptions in `simulations/data/assumptions.json` and writes `validation.json`, three CSV datasets, two LaTeX tables, and three vector figures. It covers the nested curve, daily adaptive-rate paths at the current 70% utilization target, and integer hLP recovery pricing. These are mathematical illustrations, not a full program simulator or evidence of market returns.

Both papers share their equations and parameters. After changing the shared text or data, regenerate and compile **both** papers, check references and source mappings, then render both PDFs and inspect all pages for layout problems. Update the reviewed commit and source map when the implementation snapshot changes.

No Rust, SDK, root README, or program README changes are included in this whitepaper update.
