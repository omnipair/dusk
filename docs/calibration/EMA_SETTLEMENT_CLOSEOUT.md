# EMA settlement economic closeout

Date: 2026-10-16. Runtime reviewed: `5a9abcb342aa3b1afa284b3a9613c4fa0afdebb0`.
This follow-up changes tests and documentation, not program behavior or interfaces.

## Disposition of #287652

**The direct live-spot/depth pricing defect is mitigated. The broader economic
finding remains open for explicit risk disposition.** A preceding trade does not
change the reference invoice in these tests, but can increase trading profit
around the resulting real inventory change. Do not label that result a missing
slippage check or claim that EMA eliminates liquidation-related trading risk.

No new accounting defect was confirmed in the scoped closeout review. Suppressing
all such profit would require a different execution/inventory policy, additional
trading restrictions, or an explicit economic risk limit. None is silently added.
The agreed V2 linear EMA health and EMA-price/smoothed-depth settlement remain.

## What was isolated

The new native regression opens positions through actual admission, moves the
market through native swaps, advances the real EMA, executes the implemented
emergency settlement, and searches both directions of a finite external venue.
It compares a liquidation without a preceding trade, two preceding trade sizes,
and matched sequences without liquidation. The original supplied PoC is not run.

24 starting states cover 1x/4x/10x concentration, hLP off/on, both debt directions,
and 0/50 bps token fees. Three preceding-trade sizes and liquidation/control give
144 rows, including 48 comparisons against liquidation without that trade.

**All 48 comparisons preserve the entire emergency quote and the exact per-asset
change in accounted token resources during settlement.** This includes repayment,
insurance draw, principal write-off and caller reward. The regression asserts
this in integer atoms, in addition to market invariants and full debt clearance.
It does not claim quote invariance if fee history, EMA, depth, configuration,
interest, or available insurance differs.

The remaining difference arises in subsequent AMM trading against real inventory.
The real curve is reconstructed around the changed reserves; reference reserves
are never committed. hLP ownership/funding and protected recenter buckets respond
to real reserve state. Protected amounts remain separately accounted resources,
not current LP yield or evidence of money leaving the pool.

## Concrete example

10x concentration, active hLP, quote-token debt, no token transfer fee. One common
external mark is 0.5 debt tokens per collateral token. Amounts below are debt-token
units; neither column is a prediction of production behavior.

| Measure | No preceding trade | Preceding sale of 5,000 collateral |
| --- | ---: | ---: |
| Debt repaid by reference settlement | 4,331.143 | 4,331.143 |
| Insurance draw | 500.000 | 500.000 |
| Principal written off | 2,668.857 | 2,668.857 |
| Caller reward | 43.748 | 43.748 |
| External acquisition cost | 0 | 2,571.819 |
| Proceeds from preceding Dusk sale | 0 | 1,833.866 |
| Best sampled subsequent arbitrage profit | 1,118.511 | 2,016.655 |
| Total modeled actor profit | 1,162.259 | 1,322.450 |
| Final LP/hLP claims at common mark | 72,402.056 | 70,782.604 |
| Final protected resources | 0 | 1,460.871 |
| Final accounted token resources at common mark | 72,907.699 | 72,751.518 |

The additional actor profit is **160.191**, after modeled acquisition and trading
fees. Total accounted token resources fall **156.182** relative to the no-preceding-trade
case. The approximately 1,619.453 drop in LP/hLP claims includes about 1,460.871
moved into protected reserves. It would be wrong to call all 1,619.453 an attacker
payout. The same preceding/subsequent trades without liquidation lose 528.111
(the no-preceding-trade control earns 0.036).

Stage isolation: the marked change in accounted token resources during liquidation is
+5,359.4345 in both cases. That is collateral moved into pool custody minus real
outflows, not investment profit: before liquidation these resources exclude the
position's separate collateral custody and outstanding loan. Subsequent arbitrage
reduces pool resources by 1,410.869 and 2,233.1845 respectively. This separates the
fixed settlement transfer from trading and from the removal of an impaired loan.

Across the 48 comparisons, 30 have additional modeled actor profit. Of those
30, 28 also lower LP/hLP claims and 26 also lower total marked resources. Maximum additional actor profit
is 227.088 debt tokens. These are deliberately selected stress cases, not event
probabilities, and the finite search does not establish a global optimum.

## Assumptions and limits

- External CPMM: 100,000 debt-token depth, 30 bps fee; inventory persists through
  acquisition and the subsequent trade. Dusk uses the fixture's 0.3% base fee,
  configured divergence/volatility logic, and 20% protocol swap share.
- No interest accrues during this isolated round trip. Separate default native
  and instruction tests cover accrued interest and hLP funding. This test does
  not establish outcomes across arbitrary volatility configurations.
- Includes token transfer fees, actual native swap charges and caller rewards.
  Excludes SOL transaction/priority/financing costs and keeper competition.
- This is a native economic regression, not an instruction-level exploit proof.
  It does not prove that a live bundle is feasible, nor that every configuration
  is safe. More realistic costs could erase some modeled incremental profits.
- yLP/hLP claims use the same external mark. The additional `marked_custody`
  column independently sums cash, fee custody, interest custody, backing,
  protection and insurance. It excludes outstanding external loan claims, so
  compare it with LP claims only after the tested loan is fully settled. These
  are native ledger balances, not measured token-account CPIs in this experiment.

## Reproduce

```sh
cargo test -p dusk --lib --features benchmark \
  ema_admitted_settlement_preserves_reference_and_resource_delta -- --nocapture
```

The test is `programs/dusk/src/tests/transitions/liquidation_ema_closeout.rs`.
CI runs this focused regression on every checked push/PR.
Rows prefixed `EMA_NATIVE,` reproduce [the captured CSV](ema-settlement-closeout.csv).
Raw monetary columns use 1,000 atoms per modeled token; NAV/profit/custody columns
are already token units. Side 0 means base debt; side 1 means quote debt.
Helpers for admission, venue accounting and final claims are reused from the
existing calibration modules; alternate settlement policies are not invoked.

## Liquidation progress and next decision

The current 576-combination policy sweep has 272 completed admitted paths,
288 no-entry paths and 16 paths reaching the test's shock-search cap. Completed
paths have no accepted-settlement/invariant failures or emergency-eligible
residuals. Eighteen end ordinarily eligible above the emergency boundary
(16 Dusk-only, 2 with the modeled external venue). A finite route/size sampler
cannot prove permanent blockage or universal cleanup.

Closing #287652 completely therefore requires explicit acceptance of the remaining
inventory/trading risk or a separately chosen mitigation with new validation.
The available evidence supports neither a zero-loss promise nor an undisclosed
change to the user's no-mandatory-wait, permissionless-trading policy.
