# Borrow-rate controller

This document describes the active Dusk borrow-rate controller. It supersedes
the earlier utilization-target controller described in historical research and
whitepaper snapshots.

## Pricing signal

Each debt asset has its own rate, but both rates use the same market-wide junior
capital and consolidated covered-debt values:

```text
coverage signal
  = funded junior value / consolidated covered debt value
  * (1 - asset utilization)

asset utilization
  = asset debt / (asset debt + executable asset cash)
```

Covered debt includes public loans, isolated leverage debt, and hLP funding
debt. Values use one common quote numeraire. Junior capital already queued for
exit remains funded, earns junior income, and absorbs losses until conversion,
so it remains in the signal until it actually leaves the tranche.

The default signal band is 5% through 6.25%, inclusive:

| Signal | Rate response |
| --- | --- |
| Below 5% | APR rises |
| 5% to 6.25% | APR holds |
| Above 6.25% | APR falls |

The rate doubles or halves over the configured half-life while the signal stays
outside the band. The default half-life is 14 days. Each market starts at a 4%
APR unless creation supplies another valid value. The protocol-wide minimum is
currently 1%; there is no configurable economic ceiling. With no debt, the
controller holds the current rate.

The signal band, half-life, and new-borrow utilization limit can be changed by
the market's timelocked parameter process. Initial APR is creation-only, and the
protocol-wide minimum is not a per-market parameter.

## Liquidity guards are not price targets

The controller prices risk and cash scarcity continuously. Separately, Dusk
uses two admission/execution guards:

- A new public borrow cannot make its debt asset exceed the market's configured
  utilization limit. The default and current maximum valid setting are both
  80%. Repayment, cash-backed LP withdrawal, liquidation, and cashless tranche
  conversion are not gated by this check.
- Parameter execution and concentration-ramp steps are blocked while either
  lending side is at or above 80% utilization. This is a fixed execution guard,
  not the configured new-borrow limit.

These guards do not set an APR or replace the coverage signal. Their policy and
calibration are separate product decisions.

## Source of truth

- Signal and exponential rate math:
  `programs/dusk/src/math/borrow_controller.rs`
- Market-wide junior/debt valuation:
  `programs/dusk/src/transitions/tranches/market.rs`
- Defaults, bounds, and update shape:
  `programs/dusk/src/transitions/tranches/risk_rate_model.rs`
- Interest accrual and debt accounting:
  `programs/dusk/src/transitions/lending/mod.rs`

The retained `analysis/` and `paper/` directories include dated experiments.
Their utilization-target terminology is historical unless a document explicitly
links back to this specification.
