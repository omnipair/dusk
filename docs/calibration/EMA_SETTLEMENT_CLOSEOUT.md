# EMA settlement economic closeout

Analysis: 2026-10-16. Risk acceptance: 2026-10-17. Runtime reviewed: `5a9abcb342aa3b1afa284b3a9613c4fa0afdebb0`.
The 2026-10-16 follow-up added tests and documentation. The 2026-10-17 update
records risk acceptance only; program behavior and interfaces are unchanged.

## Disposition of #287652

**Closed: the original live-spot/depth settlement dependency is remediated;
remaining inventory/arbitrage risk is explicitly accepted.** On 2026-10-17 the
user authorized closure with this request: "okay so can we close #287652 now and
document the remaining inventory/arbitrage risk as an accepted tradeoff like V1?"
This records the product owner's risk acceptance, not a new audit conclusion.

The implemented fix reconstructs emergency settlement from the collateral-side
symmetric price EMA and smoothed curve depth. V2 linear EMA eligibility remains;
current liquidity withdrawal alone does not make a healthy position liquidatable.
Size-dependent reference-curve impact and full configured fees still apply. EMA
settlement is not a flat-price or zero-slippage promise. Volatility fee history,
EMA evolution, configuration and real cash requirements remain relevant.

### Accepted tradeoff and V1 comparison

The pool accepts seized collateral in exchange for a debt offset. Its resulting
inventory can lose value, and trading before/after settlement can create
additional arbitrage profit after modeled costs. The user accepts that residual
risk with the existing permissionless trading and emergency policy. This closure
adds no waiting period, execution-price floor, route restriction, or insurance
coverage of subsequent trading losses. No new accounting defect was confirmed.

V1 also calculated an EMA-based curve exchange, canceled debt and moved seized
collateral into pool reserves. V1 used live reserve-derived depth and included
curve impact in eligibility. Dusk keeps linear V2 health, uses smoothed depth for
settlement, and has different concentration, fees and hLP accounting. The analogy
is the inventory-risk mechanism; it does not establish equal losses, identical
manipulation profitability, or historical absence of a V1 exploit. Reference:
[V1 liquidation at `84a8ef7a`](https://github.com/omnipair/omnipair-rs/blob/84a8ef7a2a3a81e750435b86d7191bf43413d6a4/programs/omnipair/src/instructions/lending/liquidate.rs).

### What insurance and socialization cover

In the example below, 7,500 principal is resolved by 4,331.143 settlement repayment,
500 insurance and 2,668.857 principal write-off. The 1,000-token insurance fund's
50% window budget binds before the 75%-of-principal-loss cap. The position's debt
is cleared. The additional 156.182 decrease in final marked pool resources is a
comparison after trading; it is not another outstanding borrower principal
balance and does not trigger a second insurance payment. LP/hLP value and
protected buckets must be distinguished from total resources. Insurance and
socialization allocate losses; they do not eliminate system-wide losses.

The acceptance covers the documented inventory/arbitrage tradeoff under the
current implementation and measured assumptions. It is not acceptance of a new
accounting defect, unauthorized debt offset, or reintroduced direct live-price/
depth dependency. It is not a zero-loss, best-execution, universal-liveness or
production-readiness claim. The measurements and limitations below are retained.

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

## Liquidation progress and closure boundary

The current 576-combination policy sweep has 272 completed admitted paths,
288 no-entry paths and 16 paths reaching the test's shock-search cap. Completed
paths have no accepted-settlement/invariant failures or emergency-eligible
residuals. Eighteen end ordinarily eligible above the emergency boundary
(16 Dusk-only, 2 with the modeled external venue). A finite route/size sampler
cannot prove permanent blockage or universal cleanup.

The 2026-10-17 acceptance above completes #287652's disposition. Ordinary-fill
availability remains a separate operational/economic limitation. Closing this
finding does not authorize merge, deployment, or a change to the existing
no-mandatory-wait, permissionless-trading policy.
