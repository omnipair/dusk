# EMA inventory settlement versus AMM liquidation

Analysis at PR #45 head `befe57fbc471c0211d9cecfc9063d30a57db98d5`, 2026-10-15.
**No runtime liquidation policy changes in this review.** This is the economic
comparison requested before the governance delegation work.

## Conclusion

At an unchanged EMA, keeping V2's symmetric-EMA eligibility prevents shrinking
LP depth alone from making an otherwise healthy loan liquidatable. The
12 healthy controls retain exactly the same collateral value, debt and maintenance
requirement after withdrawing 25% of ordinary LP shares.

V1-style EMA inventory settlement can make progress where a sale cannot meet the
ordinary flash payment. It does this by canceling a debt receivable in exchange
for collateral added to the AMM. It does **not** receive the canceled amount in
debt tokens. LPs take the inventory and its price risk. An accounting write-off of
zero is therefore not a claim of zero economic loss.

It substantially reduces the sampled manipulation advantage, but does not
eliminate it. Current AMM emergency execution also has economically profitable
manipulation cases after the modeled trading costs. Keep original finding
**#287652 open**. These are native-transition simulations, not a demonstrated
instruction-level exploit or a proof that every starting snapshot is reachable
through an admitted opening and subsequent market path.

## What was compared

384 initial market states, three front trades, four settlement choices: **4,608
results**. No initial states or front trades were excluded. Both debt directions;
1x/4x/10x concentration; with/without hLP; 1,000/15,000 collateral; 6%/4% gross
EMA equity before transfer fees; outside price equal to EMA or 20% lower; token
transfer fees 0/50 bps on both assets; ordinary LP withdrawal 0/25%.

Positions use the current stored size-margin terms captured before withdrawal.
Small cases have 7% maintenance; large cases approximately 14.61–14.63%, depending
on hLP/reference depth. They are accounting-consistent distressed snapshots,
not a replacement for the existing admission/path tests. Each comparison holds
V2 linear symmetric-EMA eligibility fixed. Price-setting trades establishing a
lower outside mark are shared exogenous history, not free attacker transactions. LP withdrawal is also a shared starting condition;
its proceeds are not counted as attacker profit.

| Choice | Settlement behavior |
| --- | --- |
| Control | Same front trade and best sampled arbitrage, with no liquidation |
| Current V2 | Native ordinary flash-to-AMM or emergency settlement, all current fees and permissions |
| V1 half | V1 same-k EMA sizing, 50% debt reduction, historical 3% collateral penalty and 0.5% collateral reward; reject if V2 health does not improve |
| V1 search | Same inventory pricing, with a sampled smaller partial permitted when half is unsuitable |

The V1 alternatives preserve V2 eligibility and improving-health checks. They
are hypothetical inventory transitions using Dusk's native accounting kernel,
**not a production implementation or approval of V1's old charges**. They do not
implement Dusk's first-recovery sizing certificate, the current 1% fee split, or
a new full-insolvency policy. All initial positions have positive EMA equity;
full V1 insolvency settlement is not tested here. This compares mechanisms and
historical sizing, not a fee-matched release candidate. The V1 reference source is
`omnipair-rs` commit `84a8ef7a2a3a81e750435b86d7191bf43413d6a4`,
`instructions/lending/liquidate.rs` and `utils/gamm_math.rs`.

The keeper chooses the most profitable sampled permitted action, including
subsequent arbitrage, in both the honest and front-traded cases. Comparing an
attacker's full close against an honest keeper forced to choose the first tiny
partial would overstate manipulation advantage; this experiment avoids that.

## Costs and valuation

- Dusk uses its native curve, base swap fee, full divergence surcharge, recenter,
  hLP reconciliation, principal netting, insurance and loss allocation.
- The protocol receives 20% of claimable swap fees in this experiment; this is
  an explicit scenario parameter. Current liquidation contributions keep their
  native 1% total and protocol/LP/insurance allocation.
- A finite outside CPMM starts with 100,000 debt tokens and equivalent collateral
  at the common mark, charging 30 bps. Its reserves persist through the front
  purchase, reward sale and reverse trade. Token transfer fees apply on each
  modeled leg. No free collateral or costless restoration trade is assumed.
- Both arbitrage directions and no trade are considered. Debt/collateral inputs
  are sampled in 500-token steps through 50,000, plus six smaller amounts. Partial
  sizes use 50/25/10/5/1/0.1/0.01% of debt or collateral as appropriate. These are
  finite searches, not global optimality proofs or live route quotes.
- LP and hLP are marked at the same exogenous price after the selected reverse
  trade. Native hLP principal valuation and fee indexes determine their claims.
  Fees are counted once. Protocol fees, protected recenter funds and insurance
  are shown separately. Conservation-only hLP backing is also recorded and is
  zero in this matrix; it is not added to redeemable hLP NAV.

An open loan is not assumed risk-free. Output includes the remaining debt and
collateral, a mark-based credit shortfall, and a separate finite-route liquidation
shortfall after transfer/venue costs. Do not add these two alternative shortfall
estimates together. For aggregate economic comparisons, subtract one from
LP claims plus protocol, protected funds and insurance. Insurance is not assumed
to cover a still-open loan merely because it has a balance.

No accrued interest, funding time, SOL transaction/priority fees, flash financing
fee, competing keeper race, or future asset decline is modeled. The position is
already distressed and its incentive age is 120 seconds; native same-slot price
updates cannot rewrite that EMA snapshot. These assumptions isolate settlement
and same-transaction manipulation economics.

## Current emergency: concrete manipulation comparison

Example: 4x concentration, hLP, 15,000 collateral, 14,400 debt, outside price = EMA
= 1 debt token per collateral, 25% ordinary LP withdrawal, no transfer fee.
Stored maintenance is 14.63%; 4% equity is already inside the emergency boundary.
Both honest and front-traded searches select a permitted full AMM sale.

| Debt-token units | Honest liquidation + arbitrage | Front trade + liquidation + arbitrage |
| --- | ---: | ---: |
| Cost to acquire front-sale collateral | 0 | 5,278.995 |
| Front sale into Dusk returns | 0 | 3,873.572 |
| Caller reward | 90.658 | 71.930 |
| Subsequent arbitrage profit | 3,664.881 | 7,983.038 |
| **Actor profit after modeled trading costs** | **3,755.539** | **6,649.545** |
| Principal repaid from sale | 8,975.221 | 7,121.139 |
| Insurance used | 500 | 500 |
| Principal written off | 4,924.779 | 6,778.861 |
| Ordinary yLP value, including fees | 56,958.428 | 54,679.990 |
| Base hLP value, including fees | 8,603.382 | 8,002.411 |
| Quote hLP value, including fees | 7,585.540 | 6,432.325 |
| Protocol fee assets | 12.900 | 17.100 |
| Protected recenter assets | 3,000.000 | 3,834.924 |
| Remaining insurance | 500 | 500 |

The same front-and-reverse trades **without liquidation lose 895.652**. Including
liquidation gives **2,894.006 more profit than the honest keeper's best sampled
liquidation**. Ordinary arbitrage and the caller reward alone do not explain it.
The caller reward actually falls; the larger reverse-trade opportunity is the
main source. Full swap fees do not prevent this result.

LP/hLP claims fall an additional 4,032.624 versus the honest liquidation. Counting
the additional protected and protocol assets reduces the loss of total modeled
pool resources to 3,193.500. This includes fees and final inventory; it is not
inferred solely from the extra 1,854.082 principal write-off.

If the actor also owns a proportional share of all three LP cohorts, their
additional profit after absorbing that ownership loss is approximately:

| Ownership of each LP cohort | Additional actor profit |
| --- | ---: |
| 0% | 2,894.006 |
| 10% | 2,490.744 |
| 50% | 877.694 |
| 100% | -1,138.618 |

This ownership sensitivity does not give the actor ownership of insurance or
protected/protocol balances. Extra financing, transaction, priority and coordination costs subtract from
these profits. If those costs are 100 debt tokens greater than for the honest
strategy, this example still has 2,794.006 extra profit. The incremental cost
break-even is 2,894.006. Capital availability and actual costs for both the front
and reverse trades must be checked in an instruction-level reproduction.

## What EMA inventory settlement changes

In the same large case, the searched EMA alternative cancels 1,440 debt and
leaves 12,960 debt against about 13,472.816 collateral. With no front trade, actor
profit is 22.874; with the 5,000-token front trade it is -528.585. That disturbance
is unattractive for this alternative. The remaining position is still liquidatable;
this is progress, not completed cleanup.

Across the full finite search, the greatest additional profit from front trading
versus the corresponding honest strategy is:

| Mechanism | Maximum extra profit, debt-token units |
| --- | ---: |
| Current V2 settlement | 2,894.006 |
| V1 fixed-half sizing, guarded by V2 health | 4.630 |
| V1 sizing with smaller partial search | 69.131 |

These maxima occur in different scenarios. They are not probabilities, guaranteed
bounds, or evidence that all forms of manipulation cost more than they return.
Residual profits in the EMA cases show that stable reference sizing alone does
not prove the post-settlement curve/hLP inventory exchange is manipulation-proof.

### A stale EMA crystallizes existing credit risk

Small example: 1,000 collateral, 940 debt, EMA = 1, outside price = 0.8, hLP,
no withdrawal or transfer fee. Ordinary V2 payment is unprofitable and emergency
is unavailable at 6% EMA equity. Historical EMA-sized half settlement cancels
470 debt and leaves 512.085 collateral with 470 debt. EMA health becomes healthy,
while the remaining outside-mark credit deficit is still 60.332.

| Value in debt tokens | Before | After EMA settlement + arbitrage |
| --- | ---: | ---: |
| Ordinary yLP | 72,012.461 | 71,958.503 |
| Base hLP | 8,114.570 | 8,100.840 |
| Quote hLP | 10,114.569 | 10,098.302 |
| Remaining loan's mark deficit | 140.000 | 60.332 |

LP/hLP claims fall about 83.954. Of that, 79.668 corresponds to reduction of the
loan's **already existing** mark deficit. After reserving for that deficit in
both snapshots, the incremental LP/hLP decline is about 4.286, not 83.954. Calling
the whole decline a new EMA-caused loss would be misleading. The pool now holds
more collateral and less debt, so future price exposure and liquidation liveness
remain separate risks.

## Partial progress and liquidity withdrawal

Of 384 no-front states, current settlement finds a sampled fill in 336, guarded
V1 half in 168, and V1 search in all 384. After this **single** selected settlement,
130/288/288 respectively remain eligible. These counts neither establish eventual
cleanup nor rank the mechanisms: current settlement can close fully, while the
EMA alternatives in these positive-equity states only reduce part of the loan.
Repeated-fill paths, current fee matching and first-recovery bounds remain needed
before selecting an EMA implementation.

V1's exact 50% rule is not a drop-in solution: it fails the improving-health check
in many larger cases. Its virtual reserve sizes still depend on live AMM depth;
withdrawals can increase seized collateral or reduce feasible partial size even
though V2 eligibility itself stays unchanged. Preserve the latter protection.

## Decision boundary

Do not change the program based only on a lower write-off number or this finite
profit search. The next product choice is whether LPs should accept an explicit
collateral-for-debt inventory settlement as an additional fallback. If selected,
keep V2 health eligibility, use the agreed current charges and partial recovery
rules, and validate repeated fills plus native instruction-level accounting and
manipulation before enabling it. This is a change to who holds the price risk.

If continuing with current AMM-only emergency settlement, its execution-price
exposure still needs mitigation or explicit acceptance; the full-fee policy and
EMA health gate have not resolved #287652. No new safeguard or loss policy is
implicitly approved by this report.

## Reproduce

```sh
cargo test -p dusk --lib --features benchmark audit_ema_inventory_ -- --nocapture > /tmp/ema-settlement.log
sed -n 's/^EMA_CMP,//p' /tmp/ema-settlement.log > /tmp/ema-settlement.csv
```

Source: `programs/dusk/src/tests/transitions/liquidation_ema_inventory_economics.rs`.
Two native tests pass: the 4,608-result comparison and 12 healthy withdrawal
controls. All accepted transitions satisfy native market invariants. Side symmetry
of actor profit differs by at most 0.003 debt tokens from rounding.
[Selected raw rows](ema-settlement-examples.csv) accompany the tables. Full output
is reproducible; no large build cache or full raw log is committed.
