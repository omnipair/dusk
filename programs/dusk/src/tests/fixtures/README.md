# hLP entry regression snapshot

`hlp-entry-devnet-500291305.bin` is the public Market account returned by an unsigned `preview_market` simulation at Solana devnet slot 500291305 for `7Rjrf8i81hZihsuFdPfzTaP6SiQ3JEmQs7Hfg7YNjkNm` (META/USDC). The reviewed program source was `1fa72d35973efedd47a157775cdd0d1ef3ae90d9`, deployed at slot 499930981. The file includes Anchor's account discriminator and contains no wallet keys.

The real META vault has 10,000,000 live shares and an actionable hedge (entry blocked); the USDC vault has no shares and admits entry. Tests replay the same accrued slot, then assert native admission and exact funding boundaries. This fixture is not production configuration or a source of current market values.

## VOB swap rounding regression snapshot

`vob-market-20260929.bin` and `vob-authority-20260929.bin` are public devnet Market and FutarchyAuthority account bytes captured at slot 505496119 for market `45qXCmfQrDxTDYc1k7Xu65Qo3kYHRYUkYmiCQKLvPBhL` (META/USDC). The block timestamp was 1790677156. They contain no private keys. Tests use this fixed state and timestamp to replay the twelve bid levels that exposed the fractional-output rounding carry, plus the opposite swap direction and an unexplained reserve-drift rejection. These fixtures are regression inputs, not current market data.

The raw captures remain unchanged as provenance. Native tests load separate
fixtures derived at the current market layout, the only one the program reads;
no deployed-account conversion runs in the loaders.

Run `node scripts/derive_operator_fixtures.cjs` after an Anchor build to reproduce
them. The script decodes the pinned layout-13 inputs at
`06094229d4a5a3858bf16bb4a2119157ca67b1cb`, then encodes the current layout
using the new IDL:
- The former 80% utilization admission threshold becomes the utilization rate
  upper target; the lower target defaults to 75%.
- The captures counted internal quantities at the nine-decimal minimum. The
  current layout counts them at `MIN_AMOUNT_DECIMALS`, so the script multiplies
  every stored curve liquidity, oracle tail, depth average, retention target,
  controller target, frozen hLP offset, hLP residual exposure and hLP NAV by
  `10^(max(12, decimals) - max(9, decimals))`. Raw token atoms, shares, prices,
  rates and per-share ratios are unchanged.
- The SDK batch fixture re-encodes each saved swap-preview return with the same
  schema and restates its retention and ordinary-reserve quantities the same
  way.

The current layout initializes the four independent continuous fee-auction
curves per side as unconfigured. Lane defaults and recipients are preserved in
`vob-authority-layout16.bin`; the superseded linear-auction parameters are
omitted. Tests use that derived authority, while the raw capture remains intact.

Historical reserve, debt, price and observation inputs otherwise remain
unchanged. These are test-data conversions, not deployed-account migrations.

| Derived fixture | SHA-256 |
|---|---|
| `vob-authority-layout16.bin` | `8352c0d9a885030d976a7384fbded2d6ebf63ef92839c54d667b8f8e45a96a2a` |
| `hlp-entry-layout16.bin` | `6b424f64806fd2b43059770942d5ca8963e7594859f5d9cb8f0aca4ce4077b54` |
| `vob-market-layout16.bin` | `5bcadea66cbda94c42d6042fd8d8df378310e1cb28a79b097675fb4180f98154` |
| `hlp-entry-funding-layout16.bin` | `b9b75e5bea1afcb9041a04fa45fbb64fb3fcc147e199c769f2984b59036cfdf2` |

## Indexed-funding hLP entry snapshot

`hlp-entry-funding-layout13.bin` is the unchanged public Market account for
`4iuoB4WsTEsv4gnwY6RZnwgaHWqYCAwCbaLswJjbwgSa`, captured on Solana devnet at slot
519889239 after the November 2026 operator release. It includes the account
discriminator and contains no wallet keys. The base vault is empty; the quote
vault has nine hLP tokens and accrued funding that requires settlement. Tests
use the derived `hlp-entry-funding-layout16.bin`, accrue that exact slot and
exercise both first and repeat deposits, coupon ownership, share-value
protection and the post-entry hedge.
