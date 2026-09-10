# Omnipair Dusk v2 Whitepaper

This directory contains two LaTeX research-paper drafts for Omnipair Dusk v2: a compact version and an expanded comparison version.

## Files

- `dusk-v2-whitepaper.tex` - compact LaTeX source.
- `dusk-v2-whitepaper-expanded.tex` - expanded LaTeX source for comparison.
- `dusk-v2-whitepaper.pdf` - rendered compact PDF, currently 10 pages.
- `dusk-v2-whitepaper-expanded.pdf` - rendered expanded PDF, currently 29 pages.
- `references.bib` - BibTeX references.
- `simulations/generate.py` - deterministic data and SVG generator.
- `simulations/data/` - generated CSV outputs.
- `simulations/figures/` - generated SVG previews and PNG versions used by LaTeX.

## Build

The PDFs were rendered locally with Tectonic:

```bash
cd paper
tectonic dusk-v2-whitepaper.tex
tectonic dusk-v2-whitepaper-expanded.tex
```

On a machine with a traditional LaTeX installation, build with one of:

```bash
latexmk -pdf dusk-v2-whitepaper.tex
latexmk -pdf dusk-v2-whitepaper-expanded.tex
```

or:

```bash
pdflatex dusk-v2-whitepaper-expanded.tex
bibtex dusk-v2-whitepaper-expanded
pdflatex dusk-v2-whitepaper-expanded.tex
pdflatex dusk-v2-whitepaper-expanded.tex
```

## Simulations

The simulation script uses only Python's standard library:

```bash
python3 simulations/generate.py
sips -s format png simulations/figures/adaptive_irm.svg --out simulations/figures/adaptive_irm.png
sips -s format png simulations/figures/slippage_depth.svg --out simulations/figures/slippage_depth.png
sips -s format png simulations/figures/hlp_tracking_loss.svg --out simulations/figures/hlp_tracking_loss.png
```

It writes deterministic CSV data and SVG previews for:

- adaptive interest-rate paths;
- constant-product slippage under different reserve-depth scales;
- hLP tracking loss as `E0 * (sqrt(r) - 1)^2`.

The paper summarizes these outputs in compact form instead of depending on SVG inclusion, so the LaTeX source remains portable.
