# Omnipair Dusk Tests

This directory contains LiteSVM smoke tests for the standalone Dusk/V2 program.

## Running

```bash
anchor build -p omnipair-v2
yarn test-litesvm
```

The test runner loads `target/deploy/omnipair_v2.so` and
`target/idl/omnipair_v2.json`, then exercises the final yLP / hLP market
architecture end to end.

## Current Suite

`v2-final-smoke.test.ts` covers:

- Market initialization with Token-2022 yLP and hLP mints.
- Balanced liquidity add/remove with floating yLP shares.
- Non-compounding yLP fee accrual, yield recipient routing, and claiming.
- Swaps, including active hLP vault checkpointing through canonical vault accounts.
- Collateral deposit/withdraw and fixed debt borrow/repay.
- hLP single-sided deposit/withdraw with aggregate vault-owned yLP and funding debt settlement.

The smoke coverage report is maintained in
`tests/utils/instruction-coverage.ts`. It tracks whether each Dusk instruction
appears in at least one LiteSVM flow. It is a checklist, not statement,
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
