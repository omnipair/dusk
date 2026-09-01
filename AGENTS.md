# Project Context

- Dusk is under active development and has not been deployed to production.
- There are no production markets, legacy deployed accounts, or migration constraints unless the user explicitly introduces them.
- Do not disable features, add production-only launch gates, or make deployment/release-scope decisions merely to close an audit finding. Surface the unresolved design risk and ask before changing product scope.
- A Cargo feature named `production` is a prospective build profile, not evidence that a production deployment exists.

# Required Pre-Commit Workflow

- Every commit must pass the same checks as `.github/workflows/ci.yaml`. Do not create a commit while a required check is failing.
- Treat changes to instruction accounts, arguments, return types, events, program IDs, or public Rust documentation as interface changes.
- After a Dusk or leverage-delegate interface change, run:
  1. `anchor build -p dusk`
  2. `anchor build -p leverage_delegate`
  3. `npm run prepare-idl --prefix packages/dusk-sdk`
  4. `npm run check:dusk-sdk`
- Commit all regenerated SDK interface files with the source change:
  - `packages/dusk-sdk/src/idl_v2.json`
  - `packages/dusk-sdk/src/types_v2.ts`
  - `packages/dusk-sdk/src/idl_delegate.json`
  - `packages/dusk-sdk/src/types_delegate.ts`
- After a faucet interface change, run `anchor build -p faucet`; its generated IDL is currently a build artifact and is not vendored into the SDK.
- Before committing, run the complete local CI sequence:
  1. `cargo fmt --all -- --check`
  2. `yarn check:hygiene`
  3. `yarn check:code-shape`
  4. `yarn check:clippy`
  5. `cargo test -p dusk`
  6. `cargo check -p dusk --lib --features production`
  7. `cargo test -p dusk --lib --features production`
  8. `cargo test -p leverage_delegate`
  9. `cargo test -p faucet`
  10. `yarn typecheck`
  11. `yarn build:litesvm`
  12. `npm run check:dusk-sdk`
  13. `DUSK_REQUIRE_COMPLETE_CU_BASELINE=1 yarn test-litesvm:no-build --forbid-pending`
