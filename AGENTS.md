# Project Context

- Dusk is under active development and has not been deployed to production.
- There are no production markets, legacy deployed accounts, or migration constraints unless the user explicitly introduces them.
- Do not disable features, add production-only launch gates, or make deployment/release-scope decisions merely to close an audit finding. Surface the unresolved design risk and ask before changing product scope.
- A Cargo feature named `production` is a prospective build profile, not evidence that a production deployment exists.

# Required Pre-Commit Workflow

- Before the first Anchor command in a clean checkout, run `node scripts/prepare_build.js`. Anchor 0.31.1 otherwise rewrites devnet program IDs when `target/deploy` is missing. The package build commands include this setup.
- `yarn install` runs `scripts/patch_anchor_idl_types.js` from `postinstall`. It rewrites Anchor's `IdlTypes` declarations to decode IDL types lazily; the eager upstream decoder exceeds TypeScript's instantiation budget on the Dusk IDL and fails `yarn typecheck` and `npm run check:dusk-sdk`. Run the script once in a checkout whose dependencies predate it.
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
      Also run `yarn check:build-identity` to verify a fresh workspace preserves program IDs.
  12. `npm run check:dusk-sdk`
  13. `DUSK_REQUIRE_COMPLETE_CU_BASELINE=1 yarn test-litesvm:no-build --forbid-pending`

# Publishing the SDK

- `.github/workflows/publish-sdk.yaml` publishes `@omnipair/dusk-sdk`; never run `npm publish` by hand. The commands are in `scripts/v2/README.md` under "Publishing the SDK".
- Deploy first, then merge. Deploy the programs the branch changes with `yarn v2:deploy-devnet` and `yarn v2:deploy-leverage-delegate-devnet`; each uploads its program's IDL from the same build through `scripts/v2/upload_idl.sh`. A program deployed any other way needs `scripts/v2/upload_idl.sh target/idl/<program>.json` before the merge. The merge to `main` then publishes the version in `packages/dusk-sdk/package.json` when it is not on npm yet and every `packages/dusk-sdk/src/idl_*.json` matches its program's IDL account on the version's cluster.
- Give every change to the package a new version: `<x.y.z>-next.<yyyymmdd>` for devnet (dist-tag `next`; `-devnet.<n>` publishes under `devnet`) and `<x.y.z>` for mainnet (`latest`). npm never takes a version twice, so a merge that keeps a published version publishes nothing.
- A merge whose IDLs do not match the cluster publishes nothing and leaves a warning on its `publish-sdk` run. Once the deployment is live, re-run that run with `gh run rerun <run-id> --repo omnipair/dusk`.
- The check compares IDLs only. Deploy a program change that keeps the IDLs before merging it; otherwise the merge publishes a package for code the cluster does not run.
- After a merge, read the `publish-sdk` run summary and `npm view @omnipair/dusk-sdk dist-tags`. A new version can return 404 for a minute or two after its publish job succeeds.
