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

The controller has two independently configurable inclusive target bands:

- combined risk signal: 5% through 6.25% by default;
- per-asset utilization: 75% through 80% by default.

Their joint response is:

| Condition | Rate response |
| --- | --- |
| Signal below 5%, or utilization above 80% | APR rises |
| Signal above 6.25%, and utilization below 75% | APR falls |
| Every other combination, including either boundary | APR holds |

Undercoverage therefore raises the rate even when demand is low. Excess
utilization raises the rate even when junior coverage is high. The APR falls
only when both underwriting and executable cash are comfortably available.

The rate doubles or halves over the configured half-life while the signal stays
outside the band. The default half-life is 14 days. Each market starts at a 4%
APR unless creation supplies another valid value. The protocol-wide minimum is
currently 1%; there is no configurable economic ceiling. With no debt, the
controller holds the current rate.

Both target bands and the half-life can be changed by the market's timelocked
parameter process. Initial APR is creation-only, and the protocol-wide minimum
is not a per-market parameter.

## Liquidity admission

New borrowing is bounded by executable cash, collateral and aggregate market
health, and the conservative-depth daily public-borrow bucket. There is no
utilization admission cap: utilization above its target raises the market rate
over the configured half-life instead of rejecting an otherwise valid loan.

Parameter execution and concentration-ramp steps remain blocked while either
lending side is at or above 80% utilization. That fixed governance execution
guard is separate from the configurable rate target and does not reject loans.

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
