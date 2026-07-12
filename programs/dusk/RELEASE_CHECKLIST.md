# Omnipair Dusk (v2) Release Checklist

Use this checklist before treating the standalone Omnipair Dusk (v2) market program as
production-ready. The root README covers the shared CI/CD and Squads deployment
mechanics; this file captures the Dusk-specific gates that must be cleared before
mainnet launch or upgrade.

## 1. Scope Freeze

- Confirm Dusk remains a standalone program: `programs/dusk`.
- Confirm repository variable `DUSK_RELEASES_ENABLED` is unset or `false`
  until this checklist and owner signoff are complete. Set it to `true` only
  for an approved release window, then set it back to `false` after the release
  artifacts are published.
- Confirm repository variable `DUSK_MAINNET_BUFFER_DEPLOYS_ENABLED` is unset or
  `false` until the approved mainnet buffer deployment window. Set it to `true`
  only while deploying a signed-off release buffer, then set it back to `false`
  after the buffer address and Squads authority transfer are recorded.
- Confirm the emergency reduce-only authority is the intended signer and can
  reach `set_reduce_only` for incident response.
- Confirm owners, dashboards, paging, and reduce-only procedures are current
  for the release.
- Confirm soft borrow and soft liquidation remain disabled unless a separate
  reviewed spec has been merged.
- Confirm LLAMMA-style liquidation, Jupiter/external aggregator conversion
  routing, explicit hedge premium pricing, user-selectable settlement side, and
  stale locked collateral-factor machinery remain out of scope unless separate
  reviewed specs have been merged.
- Confirm config updates cannot move existing effective debt below the configured
  market-health floor.

## 2. Security Review

- Run a fresh end-to-end review against the final Dusk source tree.
- Re-check the Dusk invariants in `programs/dusk/README.md`.
- Re-check the cached-spot EMA flow for same-slot manipulation resistance.
- Re-check daily borrow-limit enforcement against liquidity EMA.
- Re-check borrower risk valuation uses conservative depth
  `min(live_reserve, liquidity_ema)`.
- Confirm vanilla yLP withdrawal is not gated by daily withdrawal buckets or
  post-withdraw spot/K circuit breakers; it remains constrained by cash
  availability, user slippage bounds, pro-rata burn math, and reserve/share
  invariants.
- Re-check liquidation accounting for collateral seizure, insurance draw, and
  LP socialization.
- Re-check fee liabilities: yLP, hLP, operator, protocol, and unallocated
  carry-forward buckets.
- Re-check Token-2022 mint constraints and transfer-fee inventory accounting.

## 3. Local Verification

Run these gates from the repository root:

```bash
cargo fmt -p dusk -- --check
cargo check -p dusk --lib
cargo test -p dusk --lib -- --nocapture
cargo test -p leverage_delegate
cargo check -p dusk --lib --features production
cargo test -p dusk --lib --features production -- --nocapture
anchor build -p dusk
anchor build -p leverage_delegate
anchor build -p dusk -- --features production
npm run check-idl-current --prefix packages/dusk-sdk
npm run build --prefix packages/dusk-sdk
yarn test-litesvm
```

Release and verify-only workflows must install JavaScript dependencies with
`yarn install --frozen-lockfile` before running dusk-sdk drift or
typecheck gates.

## 4. Artifact Review

- Confirm `target/idl/dusk.json` exists and matches the intended public
  Dusk surface.
- Confirm `target/deploy/leverage_delegate.so` and
  `target/idl/leverage_delegate.json` exist before running the delegated close
  LiteSVM smoke path.
- Confirm `target/types/dusk.ts` exists and matches the same build.
- Confirm `initialize_lp_metadata` has been exercised against a real Metaplex
  Token Metadata program on the target cluster or a compatible local validator;
  the default LiteSVM smoke suite seeds LP metadata accounts directly. For the
  focused local validator path:

  ```bash
  DUSK_TEST_REAL_METADATA_CPI=1 yarn test-litesvm:no-build --grep "initializes a final yLP/hLP market"
  ```
- Confirm `packages/dusk-sdk/src/idl_v2.json` and
  `packages/dusk-sdk/src/types_v2.ts` match the latest
  `target/idl/dusk.json` and `target/types/dusk.ts` artifacts by
  running `npm run check-idl-current --prefix packages/dusk-sdk`.
- Confirm `packages/dusk-sdk/src/constants.ts` exports the intended Dusk
  program ID and PDA helpers.
- Confirm yLP and hLP Token-2022 mint constraints remain represented in both
  code and IDL-visible account flows.

## 5. Integration Readiness

- Complete the owner signoff register in
  `programs/dusk/SIGNOFF_CHECKLIST.md`.
- Review the integrator handoff in `programs/dusk/README.md` with app,
  SDK, indexer, analytics, and aggregator owners.
- SDK consumers use `IDL`, `Dusk`, and `PROGRAM_ID` or `DUSK_PROGRAM_ID`.
- Market PDA derivation uses `deriveMarketAddress`.
- Indexers consume Dusk events from the standalone Dusk IDL.
- App routing points Dusk market flows at the Dusk program ID.
- Analytics track yLP, hLP, debt, insurance, and fee state as Dusk market
  metrics.

## 6. Mainnet Deployment

- Confirm repository variable `DUSK_RELEASES_ENABLED=true` is intentionally set
  for the approved release window before publishing release artifacts.
- Confirm repository variable `DUSK_MAINNET_BUFFER_DEPLOYS_ENABLED=true` is
  intentionally set before running the mainnet buffer deploy workflow.
- For mainnet buffer deploys, use `source=release`, provide an explicit
  `release_tag`, keep `transfer_to_squads=true`, and confirm
  `SQUADS_VAULT_ADDRESS` is configured.
- Confirm `programs/dusk/src/lib.rs` declares the intended program ID.
- Build the verifiable Dusk binary with production features:

```bash
export GIT_REV=$(git rev-parse HEAD)
export GIT_RELEASE=$(git describe --tags --abbrev=0 2>/dev/null || echo "dev")

anchor build --verifiable -p dusk \
  -e GIT_REV=$GIT_REV \
  -e GIT_RELEASE=$GIT_RELEASE \
  -- --features "production"
```

- Confirm the release contains:

```text
target/verifiable/dusk.so
target/idl/dusk.json
target/types/dusk.ts
```

- Deploy the upgrade buffer through the documented workflow with `program=v2`.
- Transfer upgrade buffer authority to the configured Squads vault.
- Create and approve the Squads upgrade proposal for the Dusk program ID.

## 7. Post-Deploy Verification

- Verify the deployed Dusk binary with `solana-verify`.
- Use trailing cargo args for production verification:
  `-- --features production --config "env.GIT_REV=\"...\"" --config "env.GIT_RELEASE=\"...\""`.
- Submit the verified Dusk build to the OtterSec registry.
- Publish `@omnipair/dusk-sdk` only after the verified IDL and types
  match the deployed binary.
- Confirm the app, SDK, and indexers are using the deployed Dusk program ID.
- Smoke-test market initialization, add/remove liquidity, swap, borrow/repay,
  liquidation rejection while healthy, yield claims, protocol fee claims, and
  hLP open/close on the target cluster.
