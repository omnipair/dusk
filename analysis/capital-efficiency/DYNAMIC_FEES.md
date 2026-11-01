# Dusk with volatility and distance surcharges

**Dynamic fees can reverse the net toxic-flow comparison in some states. They
also consume part of the execution advantage.** In the illustrative warm state
below, Dusk reduces immediate LP loss by 57% versus v1 when surcharges are
distributed. If surcharges are retained for recentering, its ordinary-LP loss
is instead 74% higher than v1.

## Conditional example

Both pools start with $1m ordinary reserve capital, cash-backed and balanced at
price 1. Dusk uses 5× amplification, a 100-bps core and 400-bps additional shoulder.
Every profile uses a 30-bps base fee. Dynamic profiles additionally use:

| Control | Illustrative value |
|---|---:|
| Distance coefficient | 10 × NAD |
| Volatility coefficient | 1 × NAD |
| Distance / volatility gross-input caps | 200 / 200 bps |
| Volatility half-life | 60 seconds |
| Shock cap / accumulator cap | 0.05 / 0.10 × NAD |
| Protocol share of base fee | 20% |

The coefficients are valid illustrative controls, not an inferred current market
configuration. Quiet/warm/hot refer to the pre-trade internal accumulator at
0, 0.005 and 0.01. **This signal is not annualized volatility.** All states start
centered; the experiment conditions on an existing accumulator rather than
estimating how frequently these states occur.

### LP loss on the first +1% external-price jump

The arbitrageur optimizes trade size under the actual fee shape. Loss is the
marked inventory shortfall less the fees owned by ordinary LPs.

| Profile | Loss: surcharge distributed | Loss: surcharge retained |
|---|---:|---:|
| v1, base only | $7.12 | $7.12 |
| Dusk 5×, base only | $35.58 | $35.58 |
| Dusk 5×, dynamic, quiet | $21.20 | $26.23 |
| Dusk 5×, dynamic, warm | $3.03 | $12.35 |
| Dusk 5×, dynamic, hot | $0.00 | $0.00 |

These are **dollars per event**, not dollars per day or realized daily LVR.
Hot executes no immediate arbitrage; the quote remains stale and later fee
decay can reopen the opportunity. This is not permanent elimination of loss.

In the warm case, the chosen trade inputs about $1,816.63 and pays $5.45 base,
$9.01 volatility and $0.31 distance fees. The arbitrageur earns $1.94. The
protocol receives about $1.09, from the base fee only.

- Distributed: ordinary LP fees are $13.68; net loss is $3.03.
- Retained: ordinary LP fees are $4.36; net loss is $12.35, with $9.32 separately
  credited to the protected recenter bucket.

The retained amount is excluded from immediate yLP NAV/withdrawals. That is an
ownership/timing distinction, not a claim that the retained assets disappear.
Their eventual value depends on later admitted center moves.

### Capital efficiency after paying the fees

This is a **separate experiment**: a $10k ordinary buy, with no external price
jump, starting from the same centered curve and internal signal. Resize pool
capital until its all-in execution cost equals v1's 230.09 bps at $1m.

| Profile | Buy cost at $1m | Capital for matching v1 execution | Capital saved |
|---|---:|---:|---:|
| v1, base only | 230.09 bps | $1,000,000 | 0% |
| Dusk 5×, base only | 70.09 bps | $261,446 | 73.9% |
| Dusk 5×, dynamic, quiet | 122.50 bps | $624,195 | 37.6% |
| Dusk 5×, dynamic, warm | 172.39 bps | $737,991 | 26.2% |
| Dusk 5×, dynamic, hot | 222.28 bps | $949,256 | 5.1% |

This includes fee effects and finite-size band crossings. It replaces the
80%-capital-saving intuition when the question is this specific trade size.
The shock-loss table above still uses equal $1m capital; do not treat its losses
as losses on the resized pools in this table.

## Current formula and ordering

For input `g`, base rate `b`, pre-trade accumulator `v`, volatility coefficient
`kv`, distance coefficient `kd` and center input reserve `R0`:

```text
v_decayed = min(v, v_cap) × 2^(-elapsed/half_life)
B = b × g
pressure = kv × v_decayed
V = min((g-B) × pressure/(1+pressure), volatility_cap × g, 0.5g-B)
q_provisional = g-B-V

F(u) = 4 × kd × u^3 / [3 × R0 × (R0+u)], u >= 0
u_start = max(0, R_input-R0)
u_end   = max(0, R_input+q_provisional-R0)
D = min(F(u_end)-F(u_start), distance_cap × g, 0.5g-B-V)

q_executed = g-B-V-D
```

After a successful swap, add the capped absolute relative final price move to
the decayed accumulator. The current move does not enter its own volatility fee.
Restorative input below the center does not incur a distance charge until the
provisional path crosses the center coordinate.

The live `gross_path_divergence_fee_raw` evaluates the uncapped analytic
potential and applies gross-input budgets. **The older Huber potential/implicit
solver in test-only code is not the current execution path.** The model follows
the live gross-path ordering in real-valued arithmetic.

Fee ownership with no compounding:

```text
protocol income = protocol_base_share × B
LP fees when distributed = (1-protocol_base_share) × B + V + D
LP fees when retained    = (1-protocol_base_share) × B
protected bucket        = V + D, only when retention is active
```

## What changes the conclusion

1. **Signal decay:** independent +1% shock scenarios after 0, 30 and 60 seconds
   without trades produce distributed LP losses of $3.03, $6.98 and $10.50.
   With this half-life, the warm-state advantage disappears as the signal cools.
2. **Larger jumps:** same-slot greedy arbitrage after a +5% jump loses $365.97
   for warm Dusk, versus $275.90 for v1. Quiet Dusk takes two greedy trades and
   loses $437.44. The +1% improvement is not a universal reduction factor.
3. **Organic demand:** stronger fees narrow the price advantage. The tables
   compare quotes and capacity, not how much volume traders will actually route.
4. **Retention and center movement:** fee revenue earmarked for recentering
   needs a separate ownership ledger and a later economic admission test.
5. **Balance-sheet and strategic effects:** active hLP, cash shortages, public
   debt, globally optimized trade splitting and full price histories are not
   included in these conditional experiments.

## Reproduction and validation

From the repository root:

```sh
python3 analysis/capital-efficiency/dynamic_fees.py
python3 analysis/capital-efficiency/build_analysis.py
```

The updated full report retains the original evidence and adds the dynamic
sections. `analysis.ipynb` includes executable dynamic scenarios. Change
`FeeConfig` and the profiles in `experiments()` to explore other controls/states;
the written narrative contains reviewed defaults and must be reviewed if those
defaults change.

Checks passed for the piecewise inverse, the base-only closed-form benchmark,
buy/sell symmetry, fee ownership, total caps, an independent exact-rational
potential calculation, restorative fees, quiet volatility, half-life decay,
optimizer grid refinement and independent SQLite headline recomputation.
The model uses Python floats and ideal exponential decay, not atom-accurate
fixed-point program execution. The burst is sequential immediate-profit
maximization; it is not a worst-case strategic splitting result.

Controlling source:
[fee ordering](../../programs/dusk/src/transitions/amm/mod.rs),
[gross-path potential](../../programs/dusk/src/transitions/amm/fees.rs),
[fee ownership](../../programs/dusk/src/transitions/ledger.rs).
Fee-paying adverse selection differs from the fee-free gross benchmark; see
[Arbitrage Profits in the Presence of Fees](https://arxiv.org/abs/2305.14604).
