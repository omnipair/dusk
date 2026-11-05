# hLP entry regression snapshot

`hlp-entry-devnet-500291305.bin` is the public Market account returned by an unsigned `preview_market` simulation at Solana devnet slot 500291305 for `7Rjrf8i81hZihsuFdPfzTaP6SiQ3JEmQs7Hfg7YNjkNm` (META/USDC). The reviewed program source was `1fa72d35973efedd47a157775cdd0d1ef3ae90d9`, deployed at slot 499930981. The file includes Anchor's account discriminator and contains no wallet keys.

The real META vault has 10,000,000 live shares and an actionable hedge (entry blocked); the USDC vault has no shares and admits entry. Tests replay the same accrued slot, then assert native admission and exact funding boundaries. This fixture is not production configuration or a source of current market values.

## VOB swap rounding regression snapshot

`vob-market-20260929.bin` and `vob-authority-20260929.bin` are public devnet Market and FutarchyAuthority account bytes captured at slot 505496119 for market `45qXCmfQrDxTDYc1k7Xu65Qo3kYHRYUkYmiCQKLvPBhL` (META/USDC). The block timestamp was 1790677156. They contain no private keys. Tests use this fixed state and timestamp to replay the twelve bid levels that exposed the fractional-output rounding carry, plus the opposite swap direction and an unexplained reserve-drift rejection. These fixtures are regression inputs, not current market data.

The raw captures remain unchanged as provenance. Native tests load separate
layout-12 derived fixtures; no deployed-account conversion runs in the loaders.

Run `node scripts/derive_operator_fixtures.cjs` after an Anchor build to reproduce
them. The script decodes the pinned layout-11 inputs at
`b6cab984ae7ffef498042fb719f997633edcb11f`, then encodes layout 12 using the new
IDL. LP governance locks and configurable APR bounds are removed, and operator,
adaptive centering and concentration-ramp fields are added. Virtual reserves and
swap-preview reserve coordinates use u128. The SDK batch fixture re-encodes
each saved swap-preview return with the same schema. Historical reserve,
debt, price and observation inputs remain unchanged. The synthetic operator is
not a network authority. Old locked senior receipts are represented as external
circulating ownership in this synthetic fixture, preserving its reserve-share
denominator. These are test-data conversions, not deployed-account migrations.

| Derived fixture | SHA-256 |
|---|---|
| `hlp-entry-layout12.bin` | `7ad4466c6c043b2a30fb6e043584a2de655a75b25ee99d504cf1c49424c3c11d` |
| `vob-market-layout12.bin` | `095858f3d8450ae3853b139af39d676a30bec0fd9d8cf49442c8981ad9a902db` |
