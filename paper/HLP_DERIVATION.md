# Why 2× appears, and why Dusk's hedge can be different

**The short version:** a constant-product LP has half the local price sensitivity of its base token. Maintaining 2× gross exposure restores that sensitivity in an ideal, costless model. With concentration, the LP's token weights can change, so Dusk matches the opposite-token claim with debt; its gross leverage follows those weights.

This walkthrough accompanies the integration candidate of PR #45 at `04941f19` and PR #52 at `2f46e0e7`. The same derivation is in [the shared whitepaper](main.tex), under **From Yield Basis's square-root intuition to Dusk's hedge**. The [expanded appendix](appendices.tex) contains a historical native trade and reconstruction trace. [SOURCES.md](SOURCES.md) separates that earlier replay from the current accounting and recovery implementation.

## 1. Start with `xy = k`

Let `x` be META held by an LP slice, `y` its USDC, and `p` the USDC price of META. Ignore fees and hold constant-product liquidity fixed:

$$xy=k,\qquad p=\frac yx.$$

Substitute `y = px` into `xy = k`:

$$x=\sqrt{\frac{k}{p}},\qquad y=\sqrt{kp}.$$

The LP's total USDC value is:

$$W=px+y=2\sqrt{kp}.$$

Therefore:

$$\frac{W(p)}{W(p_0)}=\sqrt{\frac p{p_0}},\qquad \frac{dW}{W}=\frac12\frac{dp}{p}.$$

A small 1% META price move produces approximately a 0.5% LP-value move. This is the square-root intuition behind [Yield Basis's leverage explanation](https://docs.yieldbasis.com/user/protocol/compounding-leverage).

## 2. Borrowing once at 2× is not the complete result

At META = $1, contribute $100 of equity and borrow 100 USDC to hold $200 of LP assets. Initial gross leverage is `assets / equity = 200 / 100 = 2×`.

If META falls to $0.81 **without rebalancing the funding position**:

| Quantity | Start | After the price move |
| --- | ---: | ---: |
| LP assets | $200 | $200 × √0.81 = $180 |
| Debt | $100 | $100 |
| Equity | $100 | $80 |
| Value of simply holding 100 META | $100 | $81 |

The static funded LP is $1 behind holding META. For price ratio `r = p / p₀`:

$$E_{\rm static}=E_0(2\sqrt r-1),\qquad E_{\rm hold}-E_{\rm static}=E_0(\sqrt r-1)^2.$$

Its ending leverage is also `180 / 80 = 2.25×`; it did not remain at 2×.

## 3. Where continuously maintained 2× comes from

Let `n` be the number of LP units, `W` the value per unit, `D` quote debt and `E = nW − D` equity. In a costless self-financing rebalance, buying more LP units changes debt by the same value:

$$dD=W\,dn.$$

Then:

$$dE=n\,dW+W\,dn-dD=n\,dW,$$

$$\frac{dE}{E}=\underbrace{\frac{nW}{E}}_{\text{gross leverage}}\frac{dW}{W}.$$

Continuously maintain gross leverage at 2×:

$$\frac{dE}{E}=2\times\frac12\frac{dp}{p}=\frac{dp}{p},\qquad E(p)=E_0\frac p{p_0}.$$

That is the ideal one-for-one result. It assumes a smooth price path, no funding interest, no fees and costless continuous adjustment. Real execution, discrete price jumps and funding require separate accounting. Yield Basis already uses concentrated Curve collateral; the square-root benchmark is not a proof that every concentrated pool has identical 2× economics.

## 4. Concentration: use the actual inventory weights

On a fixed fee-free curve, marginal price means `p dx + dy = 0`. Differentiate total value:

$$W=px+y,\qquad dW=x\,dp+p\,dx+dy=x\,dp.$$

Thus:

$$\frac{dW}{W}=\underbrace{\frac{px}{px+y}}_{\text{META value weight }w}\frac{dp}{p}.$$

Unit local META sensitivity needs gross leverage:

$$L_{\rm META}=\frac1w=1+\frac y{px}.$$

Equivalently, borrow `y` USDC against the LP's `y` USDC claim. They cancel, leaving `x` META of net equity. For USDC-target hLP, borrow against the META claim instead; the symmetric ratio is `L_USDC = 1 + px/y`.

| META's share of LP value | META hedge leverage |
| ---: | ---: |
| 50% | 2× |
| 62.5% | 1.6× |
| 40% | 2.5× |

**The ratio can change as price moves, even without changing the concentration setting.** Price changes the inventory composition. In an arbitrage-aligned constant-product pool, `px = y`, so both value weights stay 50%. A concentrated curve does not generally preserve that equality.

This is a local sensitivity and balance-sheet identity. Dusk's finite trade and reconstruction flow must still account for all cash, fees, funding, share ownership and rounding.

## 5. A real Dusk trade: who bears the difference?

The retained [native replay](simulations/native-hlp-flow.json) executes the earlier #45 benchmark API at `5c0aca7f`, including owned hLP share settlement. Here is its fee-free concentrated case; it is an inventory example, not a quote from the integrated branch:

- Ordinary yLP starts with 100 META + 100 USDC.
- META hLP contributes 50 META; USDC hLP contributes 50 USDC.
- Concentration amplitude is 4, core half-width 100 bps and fade width 400 bps; no geometry change during this trade.
- Trader sells 22.222222222 META and receives 19.360948321 USDC.
- Final marginal price is $0.711853017 per META. This price is the comparison mark; no external price feed is simulated.

| Holder after trade/reconstruction | META claim | USDC claim | Funding debt | Net USDC value |
| --- | ---: | ---: | --- | ---: |
| Ordinary yLP | 122.222222 | 80.639052 | None | $167.643309 |
| META hLP | 50 | 32.988703 | 32.988703 USDC | $35.592651 |
| USDC hLP | 75.783519 | 50 | 75.783519 META | $50 |

META hLP's gross leverage is about **1.92684×**; USDC hLP's is **2.07894×**. Each opposite-token claim matches its funding debt. The aggregate net holder values equal the actual reserve cash valued at this same price.

Ordinary yLP's original `100 META + 100 USDC`, simply held, would be worth $171.185302 at the final price. Its actual claim is $167.643309: a **$3.541992 difference**. The trader received $19.360948321 for input worth $15.818955934 at that final mark, the same $3.541992. The matched run without hLP gives the same ordinary inventory and trade output. hLP did not create an extra unexplained debit in this comparison.

The earlier **$2** example uses a simpler shifted-product curve, not this native configuration: `(x+100)(y+100)=40,000`, starting at `(100,100)`. Selling `22.222222` META gives 20 USDC and ends at price $0.81. Ordinary LP value becomes $179 versus $181 for holding; the trader's input is worth $18 and output $20. The $2 is the finite-move trading loss against that end-price benchmark. It is distinct from the $1 static-funded CPMM tracking gap in section 2.

With fees, funding or changing geometry, those allocations change. The expanded appendix includes the historical 0.3% fee and compounding variants, including the former inventory-ratio fee conversion. The integrated branch marks that conversion at the concentrated marginal price. These examples are accounting explanations, not a guarantee of fixed token redemption or a profitability backtest.
