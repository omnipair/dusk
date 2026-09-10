# Previous Dusk whitepapers

Historical comparison copies of the July 2026 drafts, before the September 10 code-backed rewrite. These papers describe earlier mechanisms and are not the current protocol specification. The current papers remain one directory above this folder.

## Provenance and preservation

The original `paper/` bundle was absent from this checkout. The two LaTeX sources, bibliography, generator, and original README were recovered from the July 12, 2026 whitepaper drafting history. They are copied here without changing their contents; `ORIGINAL-README.md` preserves the recovered README. `source-manifest.json` records their SHA-256 hashes.

The PDFs in this folder are new renders of those recovered sources, not byte-for-byte copies of old PDF exports. The later August historical-warning banner is not part of this July source snapshot. No September mechanism corrections have been applied to these historical texts.

The old generator was run unchanged to recreate the CSV data and SVG figures. SVGs were rasterized to PNG with Sharp at 216 DPI for inclusion by the existing LaTeX sources, then both papers were compiled with Tectonic. Pagination or font rendering can differ from earlier exports. The original README's reported page counts describe its earlier build.

## Compare

| Paper | Historical render | Current September revision |
| --- | --- | --- |
| Compact | [Previous PDF](dusk-v2-whitepaper.pdf) | [Current PDF](../dusk-v2-whitepaper.pdf) |
| Expanded | [Previous PDF](dusk-v2-whitepaper-expanded.pdf) | [Current PDF](../dusk-v2-whitepaper-expanded.pdf) |

The updated papers and their supporting files were fingerprinted before this archive was created, then checked for byte-for-byte preservation. The comparison and commit receipt is in `../COMPARISON.md`.

## Rebuild

The generated PNGs are included, so either historical entry point can be rebuilt directly:

```sh
cd paper/previous
tectonic dusk-v2-whitepaper.tex
tectonic dusk-v2-whitepaper-expanded.tex
```

To regenerate the illustrative data, run `python3 simulations/generate.py`. If the SVG figures change, regenerate the corresponding PNGs before compiling. Preserve this folder as a historical snapshot rather than updating its mechanism claims.
