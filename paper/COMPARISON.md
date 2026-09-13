# Whitepaper comparison

The September 10, 2026 papers remain unchanged. Both historical July drafts are stored separately under `previous/` for comparison.

| Paper | Previous July draft | Updated September paper |
| --- | --- | --- |
| Compact | [10-page PDF](previous/dusk-v2-whitepaper.pdf) | [10-page PDF](dusk-v2-whitepaper.pdf) |
| Expanded | [29-page PDF](previous/dusk-v2-whitepaper-expanded.pdf) | [26-page PDF](dusk-v2-whitepaper-expanded.pdf) |

The previous PDFs were rebuilt from the recovered July 12 drafting sources, which were copied without editing their contents. They are historical-source renders rather than byte-original PDF exports. See [archive provenance](previous/README.md) and the [historical source hashes](previous/source-manifest.json).

All 28 files present in the updated paper bundle before this archive was created were checked for byte-for-byte preservation. The current PDFs retain these SHA-256 hashes:

```text
af99972654e03e39aed054f658b708c9f67425a3913bd104655fe908e85c8db0  dusk-v2-whitepaper.pdf
d61ad24670da411bc4f705c132c9e8b893035250ddf9a1fc6a5754d890bee183  dusk-v2-whitepaper-expanded.pdf
```

All 39 pages of the historical renders were visually reviewed. The [comparison verification record](previous/verification.json) records page counts and hashes for all four PDFs. Original historical float placement and paragraph spacing are retained.

The updated papers describe the implementation at commit `263c1635ad87ee36ffd9bf0b970457e4be965326`. See [the source map](SOURCES.md) for the substantive corrections and [the original revision verification](verification.json) for the review performed before the separate archive-and-commit request.

All 13 checks required by the repository pre-commit workflow passed before the documentation commit, including the complete LiteSVM suite. The [pre-commit verification receipt](precommit-verification.json) records the commands and results; it supplements the earlier revision review record.
