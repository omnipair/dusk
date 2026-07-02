# Omnipair Dusk Tests

This directory contains LiteSVM smoke tests for the standalone Dusk/V2 program.

## Running

```bash
yarn test-litesvm
```

`yarn test-litesvm` builds the Dusk and leverage delegate SBF artifacts before
running Mocha. Use `yarn test-litesvm:no-build --grep <pattern>` only when the
local artifacts are already fresh and you want a focused loop.

The test runner loads `target/deploy/omnipair_v2.so`,
`target/idl/omnipair_v2.json`, `target/deploy/leverage_delegate.so`, and
`target/idl/leverage_delegate.json`, then exercises the final yLP / hLP market
architecture end to end.

The default suite seeds LP metadata accounts directly so ordinary smoke runs do
not depend on a local Metaplex Token Metadata binary. Set
`OMNIPAIR_V2_TEST_REAL_METADATA_CPI=1` and optionally
`OMNIPAIR_V2_TEST_TOKEN_METADATA_PROGRAM=/path/to/metaplex-token-metadata.so`
when validating the on-chain metadata CPI with a compatible LiteSVM artifact.

## Current Suite

`v2-final-smoke.test.ts` covers:

- Market initialization with Token-2022 yLP and hLP mints.
- Balanced liquidity add/remove with floating yLP shares.
- Non-compounding yLP fee accrual, yield recipient routing, and claiming.
- Swaps, including active hLP vault checkpointing through canonical vault accounts.
- Collateral deposit/withdraw and fixed debt borrow/repay.
- Owner and delegated leverage close flows, including callback settlement.
- hLP single-sided deposit/withdraw with aggregate vault-owned yLP and funding debt settlement.

The smoke coverage report is maintained in
`tests/utils/instruction-coverage.ts`. It tracks whether each Dusk instruction
appears in at least one LiteSVM flow. The default run also reports
`initializeLpMetadata` as a known skip because that path requires a compatible
Metaplex Token Metadata program artifact. It is a checklist, not statement,
branch, invariant, or full behavioral coverage.

## Layout

```text
tests/
  v2-final-smoke.test.ts
  utils/
    instruction-coverage.ts
    litesvm-connection.ts
```

Older pair-program tests intentionally do not live in this repository.
