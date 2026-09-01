# Omnipair V2 (Dusk) Tests

This directory contains LiteSVM smoke tests for the standalone Dusk program for Omnipair V2.

## Running

```bash
yarn test-litesvm
```

`yarn test-litesvm` builds the Dusk and leverage delegate SBF artifacts before
running Mocha. Use `yarn test-litesvm:no-build --grep <pattern>` only when the
local artifacts are already fresh and you want a focused loop.

The test runner loads `target/deploy/dusk.so`, `target/idl/dusk.json`,
`target/deploy/leverage_delegate.so`, `target/idl/leverage_delegate.json`, and
the deterministic `target/deploy/token_metadata_fixture.so` fixture. The fixture
is loaded at the canonical Token Metadata program ID and validates Dusk's real
CreateV1 CPI signer/PDA boundary before creating the canonical metadata PDA.
It does not replace production Metaplex semantics.

For a focused local check, run:

```bash
yarn test-litesvm:no-build --grep "initializes a final yLP/hLP market"
```

## Current Suite

`v2-final-smoke.test.ts` covers:

- Market initialization with Token-2022 yLP and hLP mints.
- Balanced liquidity add/remove with floating yLP shares.
- Governed yLP fee compounding, non-compounded fee accrual, yield-recipient
  routing, and claiming.
- Swaps, including the canonical active-hLP remaining-account prefix:
  `[yLP mint, base hLP yLP vault, quote hLP yLP vault, base interest vault,
  quote interest vault]`, with transfer-hook extras appended afterward.
- Collateral deposit/withdraw and fixed debt borrow/repay.
- Owner and delegated leverage close flows, including callback settlement.
- hLP single-sided deposit/withdraw with aggregate vault-owned yLP, funding debt
  settlement, reserve-backing conservation, and predictive CPMM/concentrated
  swap settlement.
- The 58-instruction coverage registry includes liquidity-gate opening, hLP
  rescue and terminal close, insurance funding, and hLP order-trigger previews.

The smoke coverage report is maintained in
`tests/utils/instruction-coverage.ts`. It tracks whether each Dusk instruction
appears in at least one LiteSVM flow. It is a checklist, not statement, branch,
invariant, or full behavioral coverage.

## Layout

```text
tests/
  v2-final-smoke.test.ts
  utils/
    instruction-coverage.ts
    litesvm-connection.ts
```

Older pair-program tests intentionally do not live in this repository.
