# Omnipair V2 Integration Signoff

This document defines the evidence required before the `App/front-end routing`,
`SDK/package interface`, `Indexing/events`, `Analytics/reporting`, and
`Aggregator/router integration` rows in `SIGNOFF_CHECKLIST.md` can be marked
`Approved`. It is an integration-readiness template, not a production approval.

Use this document only for the exact release commit, program ID, IDL/types, and
target cluster being approved.

## Shared Integration Facts

Every integration owner must confirm these facts:

- Dusk is a standalone V2 program with its own program ID, IDL, account model,
  events, and SDK helpers.
- Integrations must use `PROGRAM_ID`, `OMNIPAIR_V2_PROGRAM_ID`, or
  `DUSK_PROGRAM_ID` from `@omnipair/program-interface`.
- `IDL` and `IDL_V2` are the Dusk IDL exports; `OmnipairV2` is the generated
  Anchor program type.
- Market PDA derivation uses `deriveMarketAddress` or
  `deriveMarketV2Address(baseMint, quoteMint, paramsHash)`.
- Do not sort Dusk market mints client-side. Creator-chosen `base_mint` and
  `quote_mint` define market identity and displayed price direction.
- yLP and hLP are distinct Token-2022 LP surfaces. yLP is the two-sided normal
  LP token; hLP tokens are aggregate leveraged LP vault shares.
- Token-2022 transfer hooks, fee-free LP mint constraints, and Metaplex metadata
  are part of the user-facing LP surface.
- Reduce-only mode must be visible to app, SDK, indexer, analytics, aggregator,
  and support operators.

## App And Front-End

| Area | Required evidence |
| --- | --- |
| Program routing | App PR or config diff routing Dusk flows to the Dusk program ID. |
| Market identity | UI test showing base/quote order is not client-sorted. |
| Liquidity surfaces | UI screenshots or tests for yLP add/remove and hLP deposit/withdraw flows. |
| Lending | UI or integration test for collateral deposit, borrow, repay, withdraw, and liquidation state. |
| Leverage | UI or integration test for open, close, increase, decrease, margin, delegated close, and liquidation state. |
| Claims | UI or integration test for yLP yield, hLP yield, manager fees, and protocol fee claims where applicable. |
| Reduce-only | UI test showing risk-increasing paths blocked and risk-reducing paths available. |
| Error handling | User-facing mapping for slippage, cash shortfall, risk breaker, reduce-only, and stale hLP settlement errors. |

## SDK And Package Interface

| Area | Required evidence |
| --- | --- |
| Package version | `@omnipair/program-interface` version and package diff. |
| IDL drift | `npm run check-idl-current --prefix packages/program-interface` result. |
| Typecheck | Consumer or package typecheck using `IDL`, `OmnipairV2`, and `PROGRAM_ID`. |
| PDA helpers | Tests or examples for market, reserve, collateral, fee, interest, borrow position, leverage position, yield account, insurance, futarchy authority, and metadata PDAs. |
| Transfer hook helpers | Tests or examples for Token-2022 transfer-hook validation and extra-account-meta helpers. |
| Program ID override | Test or documented behavior for `OMNIPAIR_V2_PROGRAM_ID`, `PROGRAM_ID_V2`, and `PROGRAM_ID` env overrides. |

## Indexing And Events

Indexers must subscribe to the standalone Dusk program ID and decode events from
the Dusk IDL. Each event carries `MarketEventMetadata` with signer, market, and
slot.

| Event group | Required evidence |
| --- | --- |
| Market/admin | Decoded `MarketCreated`, `MarketUpdated`, `MarketHealthUpdated`, authority/reduce-only events where applicable. |
| Liquidity | Decoded `LiquidityAdded`, `LiquidityRemoved`, `HlpOpened`, `HlpClosed`, `HlpRebalanced`. |
| Swap | Decoded `SwapExecuted` with reserve deltas, fees, and hLP pending rebalance fields where present. |
| Lending | Decoded `MarketCollateralDeposited`, `MarketCollateralWithdrawn`, `MarketDebtUpdated`, `PositionLiquidated`. |
| Yield/fees | Decoded `YieldRecipientUpdated`, `YieldClaimed`, `MarketFeeLiabilityClaimed`, `ProtocolFeesClaimed`. |
| Leverage | Decoded `LeveragePositionOpened`, `LeveragePositionClosed`, `LeveragePositionUpdated`, `LeveragePositionLiquidated`, `LeverageDelegationUpdated`. |
| Lag and failures | Dashboard or alert for indexer lag, event decode failures, and IDL mismatch. |

## Analytics And Reporting

Analytics owners must confirm metric definitions and dashboards cover:

- yLP supply, yLP backing, add/remove liquidity volume, and cash-shortfall
  rejection counts;
- hLP supply, hLP NAV, hLP funding debt, hLP live reserve, pending rebalance,
  and stale settlement rejections;
- reserve cash, live reserves, cash-backed debt, hLP live reserve, and
  `R_live = R_cash + D_cash_backed + R_hLP_live` drift;
- fee and interest vault balances versus liabilities;
- borrow utilization, borrow rates, daily borrow bucket usage, and market
  health distribution;
- liquidation count, liquidation close factor, insurance draw, and socialized
  loss;
- leverage open interest, margin health, delegated close activity, and leverage
  liquidation;
- reduce-only state, risk breaker hits, slippage failures, and integration
  error rates.

## Aggregators And Routers

| Area | Required evidence |
| --- | --- |
| Venue identity | Dusk appears as its own venue/source, not legacy Omnipair V1. |
| Quote construction | Quotes use Dusk market reserves and respect creator-chosen base/quote direction. |
| Slippage | Swap transactions always pass `min_asset_out`. |
| Depth semantics | Integration notes distinguish spot-neutral hLP depth changes from spot moves. |
| Cash constraints | Router handles cash-shortfall failures and does not assume all quoted virtual depth can leave vaults. |
| Reduce-only | Router suppresses risk-increasing routes while reduce-only is active. |
| Token-2022 | Transfer-fee or Token-2022 assets are quoted against measured inventory behavior. |
| Failure handling | Router classifies slippage, risk breaker, reduce-only, and invalid-vault errors without retry loops that worsen user execution. |

## Do Not Approve If

- any integration uses a V1 program ID, V1 IDL, V1 PDA seed, or V1 event schema
  for Dusk flows;
- a client sorts market mints before deriving a Dusk market PDA;
- yLP is displayed as fixed-principal protected LP, or hLP is displayed as a
  wrapped yLP token without funding debt/NAV context;
- indexers cannot decode Dusk events from the release IDL;
- analytics do not monitor reserve/debt drift, hLP NAV, fee-liability backing,
  liquidation, insurance, socialization, and reduce-only state;
- aggregators ignore `min_asset_out`, reduce-only, Token-2022 inventory
  behavior, or cash-shortfall failures;
- app, SDK, indexer, analytics, or aggregator owners have not acknowledged the
  final program ID, IDL, and release tag.

## Evidence Template

```text
Signoff area:
Owner:
Release commit:
Release tag:
Program ID:
IDL/type hashes:
App/staging link:
SDK/package version:
Indexer evidence:
Analytics/dashboard links:
Aggregator/router evidence:
Smoke transactions:
Known exclusions:
Open integration risks:
Approval link:
```
