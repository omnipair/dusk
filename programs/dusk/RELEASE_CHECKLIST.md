# Omnipair V2 (Dusk) Release Checklist

Use this checklist before treating the standalone Dusk market program for Omnipair V2 as
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
  reach `set_market_reduce_only` for incident response.
- Confirm owners, dashboards, paging, and reduce-only procedures are current
  for the release.
- Confirm soft borrow and soft liquidation remain disabled unless a separate
  reviewed spec has been merged.
- Confirm LLAMMA-style liquidation, Jupiter/external aggregator conversion
  routing, explicit hedge premium pricing, user-selectable settlement side, and
  stale locked collateral-factor machinery remain out of scope unless separate
  reviewed specs have been merged.
- Confirm direct-yLP parameter execution cannot run at or above 80% utilization
  and cannot retroactively apply new parameters to elapsed interest or EMA state.

## 2. Security Review

- Run a fresh end-to-end review against the final Dusk source tree.
- Reconcile every new finding into `programs/dusk/AUDIT_STATUS.md`; do not infer
  current severity from dated reports under the ignored `.audit/` directory.
- Re-check the Dusk invariants in `programs/dusk/README.md`.
- Re-check the Dusk Concentrated AMM, recentering, fee, and protected-liquidity specification in
  `programs/dusk/CONCENTRATION.md`.
- Re-check the cached-spot EMA flow for same-slot manipulation resistance.
- Re-check the per-debt-side shared 24-hour leaky/token bucket against
  pessimistic curve depth and the exact applied CPMM/Dusk Concentrated AMM risk
  shapes. Confirm checkpoint-frequency-independent refill at a fixed absolute
  limit, conservative-depth resizing, no repayment/exit refund, and enforcement
  only for public lending `borrow`. Isolated leverage and direct or automatic
  hLP funding do not consume the bucket because they do not lend cash out. It
  is not an exact trailing-window sum.
- Re-check active-hLP prediction on both CPMM and concentration bounds the combined deposited-asset
  principal plus frozen public-interest claim to
  `max(one raw target atom, 1 ppm operation-start economic NAV)` through the
  final joint correction. Confirm funding interest is excluded from both hLP
  claims, is sourced from the payer's burn legs (including exact target-side
  shortfall conversion), and cannot become an additional shared-live debit.
  Include retained surcharge and cap/fail-closed paths.
- Re-check passive interest-driven hLP insolvency and terminal loss recovery:
  `close_insolvent_hlp` must honor caller-supplied insurance/socialized-loss
  bounds and pay only the bounded caller bounty from recovered funding interest.
- Reconcile reserve custody as executable cash + swap-fee custody + both
  source-scoped hLP backing inventories. Re-run the exact 25,631-atom CPMM and
  37,886-atom concentrated conservation regressions and the indexed
  debt-share aggregate-funding boundary tests.
- Re-check public borrowing preserves debt-capped recognized aggregate
  collateral while valuing both aggregate health and position capacity on the
  pessimistic full-range-tail CPMM. Re-check lending liquidatability remains a
  linear symmetric-EMA test and floor execution uses the live concentrated
  curve.
- Re-check the lower/upper Dusk Concentrated AMM invariant bounds, exact-in/out reserve bounds,
  marginal-price proof, and low-notional fail-closed behavior.
- Re-check funded parameter ramps and center adjustments cannot consume more
  protected liquidity than admitted.
- Re-check the governed LP-owned compounded fee share enters ordinary principal,
  the remainder stays claimable, lending interest remains non-compounding, and
  only retained dynamic surcharge can create protected recentering budget.
- Re-check divergence fees are restorative-direction aware and split-resistant,
  and volatility is charged from the decayed pre-trade accumulator.
- Re-check the Huber-capped divergence marginal toll remains monotonic, its
  state potential telescopes, and its one-shot gross-path charge enforces the
  component budget without the legacy implicit fee solve. Separately re-check the volatility component budget and
  the aggregate 50% fee cap at odd/even raw-token boundaries.
- Confirm market initialization accepts only asset decimals `0..=9`, and all
  fee/quote matrix tests use that same launch domain.
- Confirm vanilla yLP withdrawal remains constrained by cash availability,
  user slippage bounds, pro-rata burn math, and reserve/share invariants.
- Re-check liquidation accounting for collateral seizure, insurance draw, and
  LP socialization.
- Confirm the liquidation policy documented in `CONCENTRATION.md`: external
  repay-and-seize bids run for five minutes, then
  `backstop_liquidation_auction` fully unwinds through the live concentrated
  curve with a 0.5% caller bounty. Test recovery cancellation, floor timing,
  AMM execution, automatic insurance limits, and automatic socialization.
- Re-check fee liabilities: yLP, hLP, protocol, and unallocated carry-forward
  buckets. Swap-fee liabilities must remain reserve-custodied
  outside executable cash; interest liabilities must remain interest-vault-custodied.
- Re-check direct-yLP governance across 1% sponsorship, strict-majority support,
  hLP-vault exclusion, seven-day timelock/window, independent family revisions,
  utilization rejection, stale/expired/cancelled paths, virtual yield, and
  exact terminal remint.
- Re-check Token-2022 mint constraints and transfer-fee inventory accounting.
- Record SBF compute units for CPMM, concentrated, retained-surcharge,
  hLP-correction, preview, leverage, and liquidation paths with explicit
  headroom below the cluster transaction ceiling.

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
npm run check-idl-current --prefix packages/dusk-sdk
npm run build --prefix packages/dusk-sdk
yarn test-litesvm
yarn test-litesvm:release
anchor build -p dusk -- --features production
DUSK_EXPECT_PRODUCTION_MINT_SUFFIXES=1 yarn test-litesvm:no-build --grep "supports the hLP launch profile"
```

The Dusk Concentrated AMM SBF swap is a mandatory gate; a native-only pass is not
sufficient. Keep the LiteSVM instruction compute report with the release
artifacts.

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
- Confirm `initialize_lp_metadata` passes the deterministic LiteSVM
  CreateV1-compatible CPI fixture and has also been exercised against the real
  Metaplex Token Metadata program on the target cluster. For the focused local
  fixture path:

  ```bash
  yarn test-litesvm:no-build --grep "initializes a final yLP/hLP market"
  ```
- Confirm `packages/dusk-sdk/src/idl_v2.json` and
  `packages/dusk-sdk/src/types_v2.ts` match the latest
  `target/idl/dusk.json` and `target/types/dusk.ts` artifacts by
  running `npm run check-idl-current --prefix packages/dusk-sdk`.
- Confirm `packages/dusk-sdk/src/constants.ts` exports the intended Dusk
  program ID and PDA helpers.
- Confirm yLP and hLP Token-2022 mint constraints remain represented in both
  code and IDL-visible account flows, including live hLP entry and withdrawal.
- Confirm the generated interface contains `initialize_yield_accounts`,
  `initialize_lp_transfer_hook`, and all five proposal-lifecycle instructions;
  `YieldAccount` contains `lp_mint`,
  `swap_fee_remainder_q64`, and `interest_remainder_q64`; and both
  `YieldClaimed` and `YieldRecipientUpdated` expose `lp_mint`. Regenerate with
  `anchor build -p dusk` and `npm run prepare-idl --prefix packages/dusk-sdk`;
  do not hand-maintain the generated JSON or TypeScript definitions.

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

- Dusk is pre-deployment. Confirm the reviewed artifact still targets fresh
  layout-v1 market genesis; migration/import behavior is outside this release.
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

- Deploy the upgrade buffer through the **Manual Dusk Buffer Deploy** workflow,
  selecting the documented `network`, `source`, release/artifact, fee, and
  Squads-transfer inputs. The workflow has no `program` input.
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
  liquidation rejection while healthy, yield claims, protocol fee claims,
  hLP entry/exit, and the full direct-yLP proposal lifecycle on the target
  cluster.
