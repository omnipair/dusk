# Native collateral leverage — local candidate

Implemented on 2026-10-02 in `dusk`, `dusk-webapp`, and `dusk-indexer`.
SDK: `2.11.0-local.native.20261002`; revision: `local-native-2026-10-02`.
This candidate has not been deployed.

## Behavior

- A META/USDC long may start with META or USDC. A short may start with
  USDC or META. Direction determines debt and collateral; the deposit selector
  only determines which asset funds the position.
- Native entry transfers the deposited collateral directly into custody, values
  it in debt units against the prepared pool price, borrows the additional
  exposure, and swaps only that borrowed amount into collateral.
- The multiplier targets leverage before fees and price impact. The preview
  reports effective leverage separately. The transaction binds maximum debt
  and minimum purchased collateral to the quote.
- Native owner-close sells the collateral needed to repay the full indexed
  debt and returns the remaining collateral to the owner. It binds maximum
  collateral sold and minimum collateral returned. Any integer-rounding
  surplus in the debt token is refunded separately and shown as debt-token dust.
- Debt-funded positions retain their existing debt-token payout on close.
- Native entry is market-only. Native limit entry, margin deposits/withdrawals,
  and TP/SL or delegated closes are not included. Size adjustments remain
  available. Liquidation retains its existing settlement behavior; the
  same-asset payout promise applies to the regular owner-close path.

## Interface and deployment boundary

The new instructions are `open_collateral_leverage` and
`close_collateral_leverage`. `LeveragePosition` adds
`funded_collateral_amount`, which is zero for debt-funded positions. Its
serialized layout changes, so existing deployed accounts cannot simply be
decoded with the new IDL. A future deployment needs an explicit account
transition or fresh market/position plan.

The collateral payout account is the first remaining account on native close,
before the hLP/transfer-hook accounts. The program checks its token program,
mint, owner, and writable privilege. The old close instruction rejects native
positions to prevent accidentally paying out in the wrong asset.

The webapp and indexer API vendor the local SDK. The indexer's active
`protocol/protocol.lock.json` remains unchanged. Candidate IDLs and hashes live
in `dusk-indexer/protocol/candidates/native-collateral/`; tests explicitly load
that candidate. The API's normal startup identity checks still reject mixing
the candidate SDK with the active devnet IDL. The webapp likewise shows a
deployment mismatch against the current devnet service. No identity checks
were bypassed and no historical data was relabeled as this candidate.

Native close requests use a 1,400,000 CU budget. The largest native close in
the local transaction matrix used 1,191,507 CU. Quote search shares the prepared
curve with execution, uses output-only curve evaluations, and is bounded to
64 bracketed interpolation steps. Unusual pool states can still reject the
quote or exceed the available compute; the tested matrix is not an audit.

## Local verification

Use Agave 4.1.0 (matching repository CI), Anchor 0.31.1, and Node 24.
Agave 3.1.15's older SBF tools hit existing concentrated-curve stack limits.
When changing SBF toolchains, remove only generated SBF/release build caches
before rebuilding; preserve program keypairs.

```sh
yarn build:litesvm
yarn check:build-identity
yarn check:dusk-sdk
yarn typecheck
DUSK_REQUIRE_COMPLETE_CU_BASELINE=1 yarn test-litesvm:no-build --forbid-pending
```

The full LiteSVM suite passes 106 tests and exercises all 68 instructions,
including both native directions with CPMM/concentrated pools, active hLP,
legacy SPL Token, and Token-2022. Native scenarios check custody conservation,
debt-cap rollback, minimum-payout rollback, wrong payout mint rejection,
legacy close rejection, debt dust refunds, empty custody after close, and
position deletion. Rust unit tests pass in default and production profiles;
SDK, TypeScript, formatting, hygiene, code-shape, and Clippy checks also pass.

Webapp protocol/leverage/feed tests pass (1,079 tests), including deployment
identity rejection and collateral/debt unit separation. Indexer API tests pass
(235) and decoder foundation tests pass (33). Browser review uses the existing
Storybook fixtures at 1280px and 390px in light and dark themes. Those fixtures
exercise UI selection and layout without a wallet or transaction submission.

Live devnet writes and a browser-to-local-validator/indexer transaction flow
have not been run. Local transaction execution is covered by LiteSVM. Native
PnL uses indexer oracle equity; when that valuation is unavailable, the UI
leaves PnL unavailable instead of treating collateral payout as debt units.
