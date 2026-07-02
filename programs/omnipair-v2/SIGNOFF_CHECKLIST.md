# Omnipair V2 Owner Signoff Checklist

Use this checklist with `RELEASE_CHECKLIST.md` before declaring the standalone
V2 market program production-ready. The local program gates can be completed by
engineering; the signoffs below require the relevant owners to review the final
branch, deployed artifacts, or target-cluster behavior.

## Signoff Register

| Area | Owner | Status | Evidence |
| --- | --- | --- | --- |
| Security review | TBD | Pending | Completed `SECURITY_REVIEW_SIGNOFF.md` with fresh review report or approval link. |
| Core invariant review | TBD | Pending | Completed `CORE_INVARIANT_SIGNOFF.md` covering GAMM, yLP, hLP, leverage, liquidation, fees, and insurance invariants. |
| Economic/risk parameters | TBD | Pending | Completed `RISK_PARAMETER_SIGNOFF.md` with assumptions, limits, and failure-mode notes. |
| Fuzzing and simulation | TBD | Pending | Completed `SIMULATION_SIGNOFF.md` evidence for adversarial hLP, leverage, liquidation, and liquidity-risk paths. |
| App/front-end routing | TBD | Pending | Completed `INTEGRATION_SIGNOFF.md` app/front-end evidence. |
| SDK/package interface | TBD | Pending | Completed `INTEGRATION_SIGNOFF.md` SDK/package evidence. |
| Indexing/events | TBD | Pending | Completed `INTEGRATION_SIGNOFF.md` indexing/event evidence. |
| Analytics/reporting | TBD | Pending | Completed `INTEGRATION_SIGNOFF.md` analytics/reporting evidence. |
| Aggregator/router integration | TBD | Pending | Completed `INTEGRATION_SIGNOFF.md` aggregator/router evidence. |
| Deployment/Squads | TBD | Pending | Completed `DEPLOYMENT_SIGNOFF.md` with buffer, proposal, approval, and execution links. |
| Release rehearsal | TBD | Pending | Completed `DEPLOYMENT_SIGNOFF.md` with dry-run release, buffer deploy, Squads transfer, verification, and rollback notes. |
| Monitoring and alerting | TBD | Pending | Dashboard and alert links mapped to `INCIDENT_RESPONSE.md` signals. |
| Incident response | TBD | Pending | Approved `INCIDENT_RESPONSE.md`, reduce-only procedure, key ceremony, contacts, and emergency runbook approval. |
| Post-deploy smoke tests | TBD | Pending | Completed `DEPLOYMENT_SIGNOFF.md` post-deploy smoke matrix with target-cluster transaction signatures. |

Allowed status values: `Pending`, `Approved`, `Blocked`, `N/A`.

## Security Review

- Use `SECURITY_REVIEW_SIGNOFF.md` as the security review evidence template.
- Confirm the reviewed source is the final standalone `programs/omnipair-v2`
  tree.
- Review the core invariants listed in `programs/omnipair-v2/README.md`.
- Review the cached-spot EMA flow and pre-action risk snapshots for swap and
  liquidity-add paths.
- Review liquidity-EMA daily borrow limits, `min(live, liquidity_ema)`
  borrower-risk depth, and spot/K circuit breakers on risk-increasing paths.
- Confirm vanilla yLP withdrawal remains governed by cash availability, slippage,
  pro-rata burn math, and reserve/share invariants, not post-withdraw spot/K
  circuit breakers or daily withdrawal buckets.
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

## Economic And Invariant Review

- Use `CORE_INVARIANT_SIGNOFF.md` as the core invariant review evidence
  template.
- Use `RISK_PARAMETER_SIGNOFF.md` as the economic/risk parameter evidence
  template.
- Confirm normal borrow, repay, interest, and liquidation paths preserve
  `R_live = R_cash + D_cash_backed + R_hLP_live` with hLP funding debt excluded
  from same-side cash-backed reserve debt.
- Confirm hLP live-reserve adjustments are explicit, balanced, spot-neutral
  within rounding, and bounded by settlement, NAV, and cash-headroom rules.
- Confirm liquidation incentives, close factors, insurance draws, LP
  socialization bounds, and dust behavior are safe under thin-liquidity and
  bad-debt scenarios.
- Confirm yLP withdrawal remains proportional, cash-constrained, and
  slippage-bounded without hidden withdrawal throttles.
- Confirm risk parameters have documented bounds, failure modes, and owner
  approval before deployment.

## Simulation And Fuzzing

- Use `SIMULATION_SIGNOFF.md` as the evidence template.
- Run adversarial sweeps for hLP rebalance, pending rebalance, leverage close,
  leverage liquidation, borrow liquidation, fee settlement, and reserve
  accounting.
- Include historical or synthetic price/liquidity paths that stress cash
  constraints, extreme utilization, stale EMA windows, dust, rounding, and
  insurance exhaustion.
- Record the commands, seeds, datasets, commit hash, and pass/fail summary used
  for signoff.

## Monitoring And Incident Response

- Use `INCIDENT_RESPONSE.md` as the runbook and evidence template.
- Confirm monitoring covers deployed program hash, upgrade authority, reduce-only
  state, reserve/debt drift, hLP NAV, pending rebalance, fee liabilities,
  liquidation events, insurance draws, LP socialization, risk EMA age, and
  indexer lag.
- Confirm emergency reduce-only keys, Squads owners, app/SDK/indexer contacts,
  and paging channels are current.
- Run a release rehearsal that toggles market reduce-only and global reduce-only
  in a controlled environment, then records transaction signatures and recovery
  steps.

## App / Front-End

- Use `INTEGRATION_SIGNOFF.md` as the app, SDK, indexer, analytics, and
  aggregator evidence template.
- Route new V2 market creation, liquidity, swap, lending, liquidation,
  insurance, yield, protocol-fee, and hedge flows to `OMNIPAIR_V2_PROGRAM_ID`.
- Do not sort V2 market mints client-side; creator-chosen base/quote order
  defines the market and displayed price direction.
- Display yLP as floating reserve-side yield LP shares.
- Display hLP as aggregate hedged LP vault shares with underlying borrowed
  debt, not as wrapped yLP.
- Surface reduce-only behavior and emergency reduce-only expectations.

## SDK / Package Interface

- Use `IDL`, `OmnipairV2`, and `PROGRAM_ID` for Dusk flows. The explicit
  `IDL_V2` and `OMNIPAIR_V2_PROGRAM_ID` aliases are also available.
- Use V2 PDA helpers from `packages/program-interface/src/constants.ts`.
- Confirm V2 IDL and generated TypeScript copies match `target/idl` and
  `target/types` artifacts from the release build.
- Confirm consumer examples use Dusk `Market` accounts.

## Indexing And Analytics

- Subscribe to the standalone V2 program ID and V2 IDL events.
- Use `MarketEventMetadata.market` as the V2 market key.
- Track yLP supply, hLP vault-owned yLP, hLP supply, hLP debt, recognized
  collateral, insurance, fee liabilities, and market health as separate V2
  metrics.
- Decode `LiquidityAdded`, `LiquidityRemoved`, `SwapExecuted`,
  `MarketDebtUpdated`, `PositionLiquidated`, yield, protocol-fee, hedge, and
  insurance events from the V2 IDL.
- Confirm analytics labels use Dusk market terminology.

## Aggregators And Routers

- Treat Dusk `swap` as its own venue/source.
- Always pass `min_asset_out` and quote with the V2 reserve floor in mind.
- Do not assume V2 yLP behaves like a fixed-principal protected LP token.
- Respect reduce-only mode and risk/circuit-breaker failures.
- Confirm Token-2022 transfer-fee assets are quoted against measured inventory
  behavior where relevant.

## Deployment And Verification

- Use `DEPLOYMENT_SIGNOFF.md` as the deployment, release rehearsal, and
  post-deploy smoke evidence template.
- Confirm repository variable `DUSK_RELEASES_ENABLED` is set to `true` only for
  an approved release window after this checklist is complete, and returned to
  `false` after release artifact publication.
- Confirm repository variable `DUSK_MAINNET_BUFFER_DEPLOYS_ENABLED` is set to
  `true` only during the approved mainnet buffer deployment window, and returned
  to `false` after recording the buffer address and Squads authority transfer.
- Confirm mainnet buffer deployments use `source=release`, an explicit
  `release_tag`, `transfer_to_squads=true`, and a configured
  `SQUADS_VAULT_ADDRESS`.
- Confirm `programs/omnipair-v2/src/lib.rs` declares the intended program ID.
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
