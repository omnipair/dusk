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

Historical reserve, debt, price and observation inputs otherwise remain
unchanged. These are test-data conversions, not deployed-account migrations.

| Derived fixture | SHA-256 |
|---|---|
| `hlp-entry-layout15.bin` | `9f1becfef00b2b90e68e3a69903512fe071f99f7a8175bb7cfa667a4996c0f47` |
| `vob-market-layout15.bin` | `ac5395115c84db6b60237dfdd5c03d9b773095e541303fe5f9aa3e5f5fe85486` |
| `hlp-entry-funding-layout15.bin` | `9972078b970c6f521f0b710485a1cf486403b4b4cfaf9fc24be7864a69c5cd05` |

## Indexed-funding hLP entry snapshot

`hlp-entry-funding-layout13.bin` is the unchanged public Market account for
`4iuoB4WsTEsv4gnwY6RZnwgaHWqYCAwCbaLswJjbwgSa`, captured on Solana devnet at slot
519889239 after the November 2026 operator release. It includes the account
discriminator and contains no wallet keys. The base vault is empty; the quote
vault has nine hLP tokens and accrued funding that requires settlement. Tests
use the derived `hlp-entry-funding-layout15.bin`, accrue that exact slot and
exercise both first and repeat deposits, coupon ownership, share-value
protection and the post-entry hedge.
