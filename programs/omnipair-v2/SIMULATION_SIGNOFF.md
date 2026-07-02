# Omnipair V2 Simulation And Fuzzing Signoff

This document defines the evidence required before the `Fuzzing and simulation`
row in `SIGNOFF_CHECKLIST.md` can be marked `Approved`. It is intentionally a
signoff template, not a claim that the work is complete.

## Current In-Repo Coverage

The release branch should run these deterministic and property-style gates from
the repository root:

```bash
cargo test -p omnipair-v2 --lib -- --nocapture
cargo test -p omnipair-v2 --lib --features production -- --nocapture
yarn test-litesvm
```

Existing property tests include:

| Area | File | Current purpose |
| --- | --- | --- |
| yLP withdrawal | `programs/omnipair-v2/src/tests/transitions/reserve.rs` | pro-rata reserve accounting, cash shortfall rejection, spot-neutral withdrawal bounds |
| borrow/repay reserve accounting | `programs/omnipair-v2/src/tests/state/market.rs` | cash-backed debt, interest, repayment rounding, virtual-reserve invariants |
| borrow liquidation | `programs/omnipair-v2/src/tests/transitions/liquidation.rs` | partial liquidation, insurance, socialization, rounded debt-share accounting |
| hLP rebalance | `programs/omnipair-v2/src/tests/transitions/hedge.rs` | target side, price move, cash headroom, pending delta, spot-neutrality bounds |

These tests are necessary release gates, but they are not sufficient by
themselves for economic signoff.

## Required Signoff Scenarios

Record commands, seeds, commit hash, input ranges, and pass/fail summaries for
each scenario.

| Scenario | Required coverage |
| --- | --- |
| hLP spot-neutral rebalance | price up/down, both target sides, cash-constrained leverage-up, pending rebalance carry-forward, hLP close after stale reference rejection |
| hLP NAV and funding | interest accrual, funding debt repayment, rounded share burn, negative-NAV rejection, settlement from borrowed-side cash |
| Borrow liquidation | close factor edges, dust, collateral exhaustion, insurance draw, LP socialization cap, thin liquidity, both debt sides |
| Isolated leverage | open/close, increase/decrease, margin add/remove, delegated close, liquidation, same-side debt accounting, bad execution bounds |
| yLP liquidity | proportional withdrawal, cash shortfall, hostile K/spot EMA settings, no hidden withdrawal throttle, minimum-liquidity lock |
| Fees and yield | fee vault/liability matching, yield recipient changes, protocol/manager claims, dust carry-forward |
| Risk books | stale EMA windows, same-slot spot manipulation attempts, `min(live, liquidity_ema)` borrower-risk depth, daily borrow-limit exhaustion |
| Token behavior | SPL Token and Token-2022 assets, transfer-fee measured inventory, LP transfer-hook constraints, metadata initialization |

## Historical Or Forked Data

When indexed Dusk or V1-like event data is available, run at least one replay or
forked simulation that includes:

- real swap-size distribution;
- real add/remove liquidity cadence;
- real borrow/repay cadence;
- stressed utilization and cash-shortfall windows;
- price paths with both smooth and jump moves;
- transaction ordering that places swaps before and after hLP rebalance
  opportunities.

Record the data source, query hash or file hash, time range, row count, and any
sampling/truncation in the signoff evidence. Do not include private keys, RPC
secrets, or user private data in committed artifacts.

## Pass Criteria

A scenario passes only if all applicable conditions hold:

- no arithmetic overflow or underflow;
- no unauthorized account, mint, or authority path succeeds;
- `R_live = R_cash + D_cash_backed + R_hLP_live` holds after every transition;
- fee and interest liabilities are backed by their vault balances;
- yLP supply remains backed by principal reserve accounting;
- hLP NAV never underflows and settlement honors cash and divergence guards;
- liquidations respect close factors, incentives, insurance, and LP
  socialization bounds;
- risk-reducing paths remain available under reduce-only;
- failures are deterministic and return expected errors.

## Evidence Template

```text
Signoff area:
Owner:
Commit:
Command:
Feature flags:
Dataset or seed:
Input ranges:
Cases:
Pass/fail:
Known exclusions:
Artifacts:
Reviewer notes:
Approval link:
```
