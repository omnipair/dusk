# hLP entry regression snapshot

`hlp-entry-devnet-500291305.bin` is the public Market account returned by an unsigned `preview_market` simulation at Solana devnet slot 500291305 for `7Rjrf8i81hZihsuFdPfzTaP6SiQ3JEmQs7Hfg7YNjkNm` (META/USDC). The reviewed program source was `1fa72d35973efedd47a157775cdd0d1ef3ae90d9`, deployed at slot 499930981. The file includes Anchor's account discriminator and contains no wallet keys.

The real META vault has 10,000,000 live shares and an actionable hedge (entry blocked); the USDC vault has no shares and admits entry. Tests replay the same accrued slot, then assert native admission and exact funding boundaries. This fixture is not production configuration or a source of current market values.

## VOB swap rounding regression snapshot

`vob-market-20260929.bin` and `vob-authority-20260929.bin` are public devnet Market and FutarchyAuthority account bytes captured at slot 505496119 for market `45qXCmfQrDxTDYc1k7Xu65Qo3kYHRYUkYmiCQKLvPBhL` (META/USDC). The block timestamp was 1790677156. They contain no private keys. Tests use this fixed state and timestamp to replay the twelve bid levels that exposed the fractional-output rounding carry, plus the opposite swap direction and an unexplained reserve-drift rejection. These fixtures are regression inputs, not current market data.

The raw captures remain unchanged as provenance. Native tests now load the
separate `hlp-entry-layout11.bin` and `vob-market-layout11.bin` derived fixtures.
They are direct layout-11 encodings: standalone coverage state and mutable fee
fields were removed, and the parameter revision list contains eight families.
There is no old-layout conversion in the native test loaders.

The intermediate account bodies were exported/adapted using the source and IDL
at `84470edcfcb4999ceb870c39853736a5bcc1b147`, then decoded and re-encoded using
the new program IDL. Added historical fields retain neutral values; existing
reserve, debt, price, funding, and observation inputs are preserved. These are
test-data conversions, not a deployed account migration or new network capture.

| Derived fixture | SHA-256 |
|---|---|
| `hlp-entry-layout11.bin` | `dc16ab3b343dec12e7b9da32b927f16d1ad2ef2a9047c309efead166c94c5700` |
| `vob-market-layout11.bin` | `a0e96ba885ec4bf77109c140a8a0746391fd6317cf5b8ae8cee275a3258271ea` |
