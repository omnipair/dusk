# Omnipair Dusk (v2) Owner Signoff Checklist

Use this checklist with `RELEASE_CHECKLIST.md` before declaring the standalone
Omnipair Dusk (v2) market program production-ready. The local program gates can be completed by
engineering; the signoffs below require the relevant owners to review the final
branch, deployed artifacts, or target-cluster behavior.

## Signoff Register

| Area | Owner | Status | Evidence |
| --- | --- | --- | --- |
| Security review | TBD | Pending | Fresh review report or approval link. |
| App/front-end routing | TBD | Pending | App PR, staging URL, or routing test notes. |
| SDK/package interface | TBD | Pending | Package diff, typed usage test, or release approval. |
| Indexing/events | TBD | Pending | Indexer config PR or decoded event sample. |
| Analytics/reporting | TBD | Pending | Metric mapping or dashboard validation notes. |
| Aggregator/router integration | TBD | Pending | Quote/swap adapter notes or integration test. |
| Deployment/Squads | TBD | Pending | Buffer, proposal, approval, and execution links. |
| Post-deploy smoke tests | TBD | Pending | Target-cluster smoke test transaction signatures. |

Allowed status values: `Pending`, `Approved`, `Blocked`, `N/A`.

## Security Review

- Confirm the reviewed source is the final standalone `programs/dusk`
  tree.
- Review the core invariants listed in `programs/dusk/README.md`.
- Review the cached-spot EMA flow and pre-action risk snapshots for swap and
  liquidity-add paths.
- Review liquidity-EMA daily limits and spot/K circuit breakers.
- Review floating yLP liquidity, matched yLP redemption, and Token-2022
  transfer checkpointing.
- Review fee liabilities and settlement paths for yLP, hLP, operator,
  protocol, and unallocated buckets.
- Review fixed debt, recognized collateral, normalized valuation, and
  liquidation/insurance/socialization accounting.
- Review Token-2022 constraints and measured inventory-credit settlement.
- Confirm soft borrow and soft liquidation remain disabled unless a separate
  reviewed spec has been merged.
- Confirm LLAMMA-style liquidation, Jupiter/external aggregator conversion
  routing, explicit hedge premium pricing, user-selectable settlement side, and
  stale locked collateral-factor machinery remain out of scope unless separate
  reviewed specs have been merged.

## App / Front-End

- Route new Dusk market creation, liquidity, swap, lending, liquidation,
  insurance, yield, protocol-fee, and hedge flows to `DUSK_PROGRAM_ID`.
- Do not sort Dusk market mints client-side; creator-chosen base/quote order
  defines the market and displayed price direction.
- Display yLP as floating reserve-side yield LP shares.
- Display hLP as aggregate hedged LP vault shares with underlying borrowed
  debt, not as wrapped yLP.
- Surface reduce-only behavior and emergency reduce-only expectations.

## SDK / Package Interface

- Use `IDL`, `Dusk`, and `PROGRAM_ID` for Dusk flows. The explicit
  `IDL_V2`, `OmnipairV2`, and `OMNIPAIR_V2_PROGRAM_ID` aliases are also available.
- Use Dusk PDA helpers from `packages/dusk-sdk/src/constants.ts`.
- Confirm Dusk IDL and generated TypeScript copies match `target/idl` and
  `target/types` artifacts from the release build.
- Confirm consumer examples use Dusk `Market` accounts.

## Indexing And Analytics

- Subscribe to the standalone Dusk program ID and Dusk IDL events.
- Use `MarketEventMetadata.market` as the Dusk market key.
- Track yLP supply, hLP vault-owned yLP, hLP supply, hLP debt, recognized
  collateral, insurance, fee liabilities, and market health as separate Dusk
  metrics.
- Decode `LiquidityAdded`, `LiquidityRemoved`, `SwapExecuted`,
  `MarketDebtUpdated`, `PositionLiquidated`, yield, protocol-fee, hedge, and
  insurance events from the Dusk IDL.
- Confirm analytics labels use Dusk market terminology.

## Aggregators And Routers

- Treat Dusk `swap` as its own venue/source.
- Always pass `min_asset_out` and quote with the Dusk reserve floor in mind.
- Do not assume Dusk yLP behaves like a fixed-principal protected LP token.
- Respect reduce-only mode and risk/circuit-breaker failures.
- Confirm Token-2022 transfer-fee assets are quoted against measured inventory
  behavior where relevant.

## Deployment And Verification

- Confirm `programs/dusk/src/lib.rs` declares the intended program ID.
- Build the verifiable binary with production features and embedded
  `GIT_REV`/`GIT_RELEASE` metadata.
- Deploy the upgrade buffer through the documented Dusk buffer workflow.
- Transfer upgrade buffer authority to the configured Squads vault.
- Create, approve, and execute the Squads upgrade proposal.
- Verify the deployed binary with `solana-verify` using trailing cargo args for
  `--features production` and the release metadata config.
- Submit the verified build to the OtterSec registry.

## Post-Deploy Smoke

Record target-cluster transaction signatures for:

- market initialization;
- add liquidity and remove liquidity;
- claim yLP and hLP yield;
- swap with slippage protection;
- deposit collateral, borrow, repay, and withdraw idle collateral;
- healthy liquidation rejection;
- unhealthy liquidation on a controlled test market;
- insurance-backed liquidation path;
- deposit single-sided liquidity and withdraw single-sided liquidity;
- reduce-only mode rejection for risk-increasing paths.
