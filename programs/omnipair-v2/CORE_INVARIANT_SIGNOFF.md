# Omnipair V2 Core Invariant Signoff

This document defines the evidence required before the `Core invariant review`
row in `SIGNOFF_CHECKLIST.md` can be marked `Approved`. It is a review
template, not a claim that the invariants below are already proven for a
release.

Use this document against the exact release commit, generated IDL/types, and
deployment artifacts. Green tests are required evidence, but they are not a
substitute for reviewer signoff against the final code.

## Canonical Invariant Set

The reviewer must confirm that each invariant is stated, implemented, tested,
and monitored where applicable.

| Area | Invariant | Primary code evidence | Required review evidence |
| --- | --- | --- | --- |
| GAMM reserve accounting | For each side, `R_live = R_cash + D_cash_backed + R_hLP_live` after every transition. hLP funding debt is not same-side cash-backed reserve debt. | `Market::assert_virtual_reserve_invariant` in `src/state/market/mod.rs` | Code review notes for borrow, repay, swap, yLP, hLP, leverage, liquidation, and interest paths. |
| Constant-product pricing | Swaps and reserve mutations preserve the intended GAMM pricing model and do not introduce unaccounted virtual depth. | `swap_reserves`, GAMM math, hLP rebalance transitions | Reviewer notes showing which paths change spot, depth, or both. |
| yLP backing | yLP supply is backed by paired base/quote principal reserves. No operation mints yLP without corresponding reserve value. | `add_liquidity`, `remove_liquidity`, `assert_share_backing` | Review of mint/burn math, minimum-liquidity lock, pro-rata withdrawals, and cash-shortfall rejection. |
| Fee separation | yLP principal reserves exclude fee and interest vault balances. Fee and interest liabilities are backed by vault balances. | `Fees::assert_backed`, `record_swap_fee_credit`, interest-credit paths | Review of yLP, hLP, manager, protocol, buyback, and unallocated fee buckets. |
| hLP NAV | hLP NAV is `collateral_value - debt_value`, must not underflow, and share minting uses NAV deltas conservatively. | `hlp_nav_nad`, hLP open/close/rebalance transitions | Review of positive NAV, negative NAV rejection, rounded share mint/burn, and close settlement. |
| hLP live reserve | hLP live-reserve adjustments are explicit, balanced, and bounded by settlement, NAV, and borrowed-side cash headroom. | `HlpVault::{credit,debit}_hlp_live_reserve`, `rebalance_hlp_vaults` | Review of leverage-up, deleverage, pending rebalance, stale reference rejection, and spot-neutrality bounds. |
| hLP debt | hLP funding debt accrues interest and counts toward utilization, but is tracked as aggregate hLP vault debt, not borrower debt or yLP-denominated debt. | `HlpVault::clear_debt_repay`, debt accrual, hLP funding paths | Review of debt shares, principal, realized interest split, and debt clearance rounding. |
| Borrower health | Market health uses recognized debt-bearing collateral for borrower debt; idle collateral contributes zero. Config updates cannot make existing effective debt unsafe. | `market_health`, `assert_market_health_snapshot`, `apply_config_update` | Review of recognized collateral, `min(live, liquidity_ema)` depth, and health floor enforcement. |
| Borrow liquidation | Liquidation uses reference pricing without internal AMM execution and follows borrower collateral, liquidator incentive, insurance, then bounded LP socialization. | `settle_liquidation`, liquidation transitions | Review of close factor, incentive curve, dust, collateral exhaustion, insurance draw, and socialized loss. |
| Isolated leverage | Isolated leverage debt contributes to utilization and interest without entering normal borrower health. Collateral vault balances match open leverage position accounting. | leverage state and transitions | Review of open, close, increase, decrease, margin add/remove, delegated close, and liquidation paths. |
| Risk books | EMA books update from cached pre-transition observations and store current observations for the next refresh. Risk-increasing paths use the intended circuit breakers. | `Risk::refreshed`, risk/health helpers | Review of same-slot manipulation resistance, stale EMA windows, spot/K checks, and borrow limits. |
| Token constraints | yLP and hLP mints are fee-free Token-2022 mints with transfer hooks, no freeze authority, market mint authority, vanity suffixes, and zero supply at initialization. | `validate_lp_mint`, `initialize_lp_metadata` | Review of mint validation, metadata CPI path, transfer-hook checkpointing, and Token/Token-2022 inventory accounting. |
| Delegated leverage close | Delegated close validates both delegate approval and settlement approval return data and binds them to the expected market, position, recipient, and output. | leverage delegate instruction paths | Review of callback account validation, payload binding, and failure behavior. |
| Reduce-only liveness | Reduce-only blocks risk-increasing paths while keeping risk-reducing paths available. | `set_reduce_only`, instruction guards | Review of emergency operation and incident response assumptions. |

## Required Evidence

Before approval, attach or link:

- final release commit hash;
- generated IDL and TypeScript artifact hashes;
- reviewer notes for every row in the canonical invariant set;
- commands and results for unit, production-feature, LiteSVM, and
  program-interface gates;
- relevant `SIMULATION_SIGNOFF.md` evidence for adversarial cases;
- relevant `RISK_PARAMETER_SIGNOFF.md` evidence for economic assumptions;
- notes for any invariant that depends on monitoring or incident response;
- explicit list of reviewed exclusions and disabled features.

## Current In-Repo Assertion Hooks

The following hooks are useful evidence, but do not prove coverage by
themselves:

| Hook | Purpose |
| --- | --- |
| `Market::assert_market_invariants` | Runs share backing, fee backing, and both side reserve-accounting checks. |
| `Market::assert_virtual_reserve_invariant` | Enforces `R_live = R_cash + D_cash_backed + R_hLP_live` per side. |
| `Fees::assert_backed` | Requires fee and interest vault balances to cover all fee liabilities. |
| `MarketSide::assert_share_backing` | Checks yLP share backing against principal reserves. |
| `Market::assert_market_health_snapshot` | Checks market health against the configured health floor. |

Record any invariant that is reviewed manually rather than covered by a runtime
assertion, and explain why runtime assertion is impractical or unnecessary.

## Required Test And Simulation Coverage

The release evidence must include:

- deterministic and property tests for borrow/repay reserve accounting;
- hLP open, close, rebalance, pending rebalance, cash-headroom, and stale
  settlement tests;
- yLP add/remove liquidity tests for mint/burn math, proportional withdrawal,
  cash shortfall, and hidden-throttle absence;
- borrow liquidation tests for close factor, dust, insurance, socialization,
  collateral exhaustion, both debt sides, and rounding;
- leverage tests for open, close, increase, decrease, margin, delegated close,
  liquidation, and socialized loss;
- fee/yield tests for vault/liability backing, manager/protocol claims, and
  unallocated dust;
- Token-2022 and metadata tests for LP mints, transfer hooks, and real Metaplex
  CPI coverage;
- LiteSVM smoke coverage for critical user and admin flows.

## Do Not Approve If

- any path can change `R_live` without matching cash, cash-backed debt, or hLP
  live-reserve movement;
- hLP funding debt is counted as normal same-side cash-backed borrower debt;
- hLP can mint shares from stale, negative, or unbounded NAV;
- yLP withdrawals are blocked by hidden risk throttles instead of cash,
  slippage, pro-rata burn math, and reserve/share invariants;
- fee or interest liabilities can exceed their vault balances;
- liquidation requires an internal AMM swap to make borrower debt accounting
  safe;
- config updates can leave an already unhealthy effective-debt state;
- disabled features such as soft borrow, soft liquidation, LLAMMA-style
  liquidation, Jupiter/external aggregator conversion routing, explicit hedge
  premium pricing, user-selectable settlement side, or stale locked
  collateral-factor machinery are merged without separate review.

## Evidence Template

```text
Signoff area:
Owner:
Commit:
IDL/type artifacts:
Reviewed invariant rows:
Commands:
Simulation evidence:
Risk parameter evidence:
Manual review notes:
Runtime assertions relied on:
Known exclusions:
Open questions:
Approval link:
```
