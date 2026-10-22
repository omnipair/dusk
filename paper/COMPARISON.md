# Whitepaper comparison

The October 23 revision updates the shared mechanism text for the #45 + #52 integration candidate. Both historical July drafts remain unchanged under `previous/`.

| Paper | Previous July draft | Current October paper |
| --- | --- | --- |
| Compact | [10-page PDF](previous/dusk-v2-whitepaper.pdf) | [Current PDF](dusk-v2-whitepaper.pdf) |
| Expanded | [29-page PDF](previous/dusk-v2-whitepaper-expanded.pdf) | [Current PDF](dusk-v2-whitepaper-expanded.pdf) |

The previous PDFs were rebuilt from the recovered July 12 drafting sources, which were copied without editing their contents. They are historical-source renders rather than byte-original PDF exports. See [archive provenance](previous/README.md) and the [historical source hashes](previous/source-manifest.json).

The first September revision (10 and 26 pages) and both July comparison drafts were preserved in commit `d9efadc`. All 28 files in that September bundle were checked for byte-for-byte preservation when the archive was created. Those earlier September PDFs had these SHA-256 hashes:

```text
af99972654e03e39aed054f658b708c9f67425a3913bd104655fe908e85c8db0  dusk-v2-whitepaper.pdf
d61ad24670da411bc4f705c132c9e8b893035250ddf9a1fc6a5754d890bee183  dusk-v2-whitepaper-expanded.pdf
```

All 39 pages of the historical renders were visually reviewed. The [archive verification record](previous/verification.json) records page counts and hashes as they stood at commit `d9efadc`. Original historical float placement and paragraph spacing are retained. The current PDF hashes, page counts, and checks are in [the current verification record](verification.json).

The October 19 revision added the full square-root/2×/inventory-weight derivation, native trade and hLP reconstruction examples, and replaced obsolete auction and fixed-margin descriptions. This revision incorporates #52's full-value hLP accounting, cash-funded harvest, spot cash carry and flash recovery. The retained numerical replay is explicitly pinned to the earlier #45 source; see [SOURCES.md](SOURCES.md).

The historical `previous/` bundle remains unchanged. The [October 19 pre-commit receipt](precommit-verification.json) belongs to that earlier documentation update; combined-branch validation is separate.
