# Omnipair V2 Risk Parameter Signoff

This document defines the evidence required before the `Economic/risk
parameters` row in `SIGNOFF_CHECKLIST.md` can be marked `Approved`. It is a
signoff template, not a claim that the parameters below are final or safe.

Use this document for the exact release commit, target cluster, and market set.
Do not reuse approvals across commits, feature flags, or materially different
asset pairs without recording why the assumptions still hold.

## Current On-Chain Bounds

These bounds are enforced by `MarketConfig::validate()` or constants in
`programs/omnipair-v2/src/constants.rs`. They are necessary guardrails, not
economic approval.

| Parameter | Code-enforced bound | Owner signoff focus |
| --- | --- | --- |
| `swap_fee_bps` | `0..=10_000` | Fee level, routing competitiveness, fee-vault growth, hLP/yLP revenue assumptions |
| `manager_fee_bps` | `0..=500` | Manager take rate, LP net yield, governance or manager incentive alignment |
| `protocol_fee_bps` | Must be `0` in `MarketConfig` | Protocol revenue is currently controlled through futarchy revenue share, not per-market config |
| `target_hlp_leverage_bps` | Must be `20_000` | hLP is fixed to lambda=2 unless a separate reviewed design changes it |
| `settlement_divergence_bps` | `0..=10_000` | hLP stale-reference and settlement rejection tolerance |
| `emergency_exit_haircut_bps` | `0..=10_000` | User exit loss bound during emergency paths |
| `ema_half_life_ms` | `60_000..=43_200_000` | Spot EMA responsiveness and manipulation resistance |
| `directional_ema_half_life_ms` | `60_000..=43_200_000` | Directional price-memory behavior for risk checks |
| `k_ema_half_life_ms` | `60_000..=43_200_000` | Liquidity/K memory used by borrow limits and circuit breakers |
| `max_daily_borrow_bps` | `0..=10_000` | Borrow throttle against liquidity EMA and launch liquidity assumptions |
| `spot_ema_divergence_bps` | `0..=10_000` | Maximum tolerated spot/EMA dislocation on risk-increasing paths |
| `k_ema_drawdown_bps` | `0..=10_000` | Maximum tolerated K drawdown on risk-increasing paths |
| `recognized_collateral_cap_bps` | `>=10_000` and `>= market_health_min_bps` | Maximum recognized collateral versus effective debt under oracle-less pricing |
| `market_health_min_bps` | `>=10_000` and `<= recognized_collateral_cap_bps` | Borrow liquidation threshold and post-config-update health floor |
| `hedged_lp_enabled` | Boolean | Whether hLP deposit/rebalance paths are enabled for the market |
| `start_time` | `i64` timestamp | Launch timing, app routing, and integration readiness |
| `LIQUIDATION_CLOSE_FACTOR_BPS` | Constant `5_000` | Partial liquidation cadence and dust behavior |
| `LIQUIDATION_INCENTIVE_BPS` | Constant `100` | Starting liquidator incentive |
| `LIQUIDATION_MAX_INCENTIVE_BPS` | Constant `500` | Maximum dynamic liquidator incentive under low health |
| `LIQUIDATION_INSURANCE_FUNDING_BPS` | Constant `200` | Insurance funding from liquidation penalty |
| `LIQUIDATION_PENALTY_BPS` | Constant `300` | Borrower penalty split across liquidator and insurance |
| `LEVERAGE_MAX_MULTIPLIER_BPS` | Constant `200_000` | Hard leverage circuit breaker |
| `LEVERAGE_MAX_UNWIND_IMPACT_BPS` | Constant `200` | Maximum allowed leverage unwind impact |
| `LEVERAGE_INITIAL_MARGIN_BPS` | Constant `1_000` | Initial margin floor |
| `LEVERAGE_MAINTENANCE_BUFFER_BPS` | Constant `700` | Maintenance buffer before leverage liquidation |
| `MARKET_GOVERNANCE_DELAY_SLOTS` | Constant `216_000` | Config and authority timelock duration |

## Launch Parameter Matrix

Create one row per market or asset class. Do not approve a market whose risk
profile is only implied by another market unless the owner explicitly signs that
the same assumptions apply.

| Market or class | Config values | Asset assumptions | Liquidity assumptions | Borrow demand assumptions | hLP enabled | Evidence | Owner | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| TBD | TBD | TBD | TBD | TBD | TBD | TBD | TBD | Pending |

Allowed status values: `Pending`, `Approved`, `Blocked`, `N/A`.

## Required Evidence

Each approved row should include:

- the release commit hash and generated IDL/type artifact hashes;
- the exact `MarketConfig` values to initialize or update;
- target mints, decimals, Token or Token-2022 behavior, and transfer-fee notes;
- minimum expected launch liquidity and maximum expected single borrow/swap
  sizes;
- expected utilization bands and borrow-rate behavior under stressed demand;
- liquidation close-factor, incentive, insurance, and socialization assumptions;
- hLP cash-headroom, settlement-divergence, and stale-reference assumptions;
- circuit-breaker tolerance for spot/EMA and K/EMA dislocations;
- references to relevant `SIMULATION_SIGNOFF.md` scenarios and seeds;
- owner notes for why the chosen parameters are conservative enough.

## Failure-Mode Review

Record owner decisions for each failure mode. A blank answer is not approval.

| Failure mode | Required decision |
| --- | --- |
| Thin liquidity with large borrow demand | Confirm `max_daily_borrow_bps`, `min(live, liquidity_ema)` depth, and market-health floor prevent unsafe debt growth. |
| Sudden liquidity exit | Confirm yLP withdrawal remains cash-constrained and that borrowers remain liquidatable through seize-and-repay accounting. |
| Spot/EMA divergence | Confirm risk-increasing paths reject at the chosen `spot_ema_divergence_bps` before bad debt can be manufactured. |
| K/EMA drawdown | Confirm risk-increasing paths reject at the chosen `k_ema_drawdown_bps` without becoming a hidden yLP withdrawal throttle. |
| hLP stale reference | Confirm `settlement_divergence_bps` rejects unsafe hLP settlement while keeping normal close/rebalance liveness. |
| hLP cash headroom exhaustion | Confirm pending rebalance behavior, NAV accounting, and close paths are acceptable under stressed cash. |
| Insurance exhaustion | Confirm liquidation socialization behavior and user-facing disclosure. |
| High utilization and stale accrual | Confirm adaptive interest bounds and accrual caps do not create unsafe debt-index jumps. |
| Transfer-fee or Token-2022 asset | Confirm measured inventory accounting and LP transfer-hook expectations. |
| Authority or release mistake | Confirm reduce-only authority, governance delay, Squads ownership, and incident-response procedure. |

## Evidence Template

```text
Signoff area:
Owner:
Commit:
Market or class:
Base mint:
Quote mint:
Config values:
Constants reviewed:
Simulation evidence:
Historical/forked evidence:
Liquidity assumptions:
Borrow assumptions:
hLP assumptions:
Liquidation assumptions:
Known exclusions:
Failure-mode notes:
Approval link:
```
