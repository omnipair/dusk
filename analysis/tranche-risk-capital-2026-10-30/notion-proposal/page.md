> Historical cofounder proposal. The implemented specification now uses hLP, yLP and jrLP; see [the current plan](../PLAN.md) and [implementation evidence](../IMPLEMENTATION.md). Earlier open questions below record the original discussion.

## Problems:

### 1. V2 improved the borrower experience, but left a gap in how we price risk

In V1, we valued collateral based on what the pool could recover after selling it, including the price impact of that sale. That gave LPs protection against thin liquidity. But it also meant a borrower's liquidation threshold could move when other people withdrew liquidity, even if the borrower had done nothing.

We removed that dependency in V2 to improve capital efficiency and make borrowing and leverage more predictable. I think that was the right direction. The gap is that we did not replace that particular protection with an interest rate that responds to how much risk LPs are willing to cover.

V2 still has collateral requirements, liquidations and insurance. The missing piece is a price for the remaining lending risk. Utilization tells us how much liquidity is being borrowed. It does not tell us how much capital is willing to take the first loss if a borrower leaves bad debt.

### 2. Our insurance capital sits outside the productive pool

The current insurance vault keeps tokens available to cover losses. That buffer has a purpose, but the money sitting there is not also providing AMM liquidity.

We are asking capital providers to fund the market and a separate reserve beside it. The proposal is to make part of the capital already in the market responsible for covering losses, and pay its owners for accepting that responsibility.

## Proposed solution

Give LPs two choices within the same pool:

| | Senior | Junior |
| --- | --- | --- |
| What the LP chooses | Lower exposure to lending losses | Take lending losses first |
| What they earn | A smaller interest return per dollar at the intended pool mix | A larger interest return per dollar for underwriting |
| What happens after collateral recovery | Junior absorbs bad debt before senior | Junior absorbs bad debt up to its remaining capital |

Both portions remain productive LP capital. The split changes who gets paid and who absorbs lending losses. It does not require moving all the junior capital into an idle cash reserve.

This addresses the two problems together. We keep V2's more predictable borrower rules and add a market signal for pricing uncovered exposure. We also reduce the need to fund a separate idle insurance reserve, because junior LPs supply the first layer of protection from inside the pool.

{{CAPITAL_IMAGE}}

Senior is lower risk relative to junior for covered lending losses. Both still carry the risks of the underlying LP position, including token price changes. If bad debt exceeds the junior buffer, senior takes the remaining loss. Productive junior capital can also lose value during the same market move that creates bad debt, so the coverage ratio must use its current value.

## Why use risk coverage to set interest?

Imagine two pools with the same amount of borrowing and the same utilization. One has substantial junior capital behind its loans; the other has almost none. A utilization-only controller sees the same input, even though the protection available to senior LPs is very different.

Our proposed signal is simple:

**Committed junior capital / covered debt**

If there is $20 of committed junior capital behind $80 of debt, coverage is 25%. The controller compares that ratio with a target range:

- Below the range, borrowing rates rise over time. Junior earns more interest if borrowers keep paying, which can attract underwriting capital. Borrowers can also repay and reduce the exposure.
- Inside the range, the rate holds.
- Above the range, the rate falls toward a minimum.

This is the risk-oracle idea: LPs express their view by committing capital that can take losses. The protocol uses that participation to adjust the price. It measures willingness to underwrite at the offered terms, rather than calculating an exact probability of default.

That makes it a better fit for this problem than utilization alone. It responds directly when underwriting capital leaves, even if borrowing has not changed. It also lets risk-tolerant LPs earn more for supplying the protection that other LPs want.

Liquidity still needs its own rule. We can test a cap that rejects new borrowing above 80% utilization. Withdrawals still require available tokens. A senior withdrawal does not directly change junior capital per dollar of debt, so it should not automatically move the coverage-driven borrow rate. It does reduce cash available for the next withdrawal.

The 80% cap is a starting point to test, not a promise that 20% cash will always remain. Withdrawals can push utilization above it. When the cap binds, borrowing is limited by availability at the current rate.

## How LPs get paid

Start with a fixed 50/50 split of net collected borrowing interest between junior and senior. Ordinary swap fees stay proportional to LP ownership.

Equal revenue does not mean equal yield. If junior supplies 20% of the pool and senior supplies 80%, each group receives half the interest, but junior shares its half across much less capital.

For example, with $100 in the pool, $80 borrowed at 8%, and all interest paid, annual borrowing income is $6.40 before protocol fees. Junior receives $3.20 on $20, or 16%. Senior receives $3.20 on $80, or 4%. These are illustrative APRs, before swap fees and losses.

The incentive adjusts as people move. More junior capital spreads its half of the income across more LPs. Less junior capital concentrates that income among the LPs who remain. The borrow-rate controller responds to the changing coverage as well.

The split does not guarantee junior always has a higher quoted yield. If junior becomes larger than senior, an equal revenue split can give junior a lower return per dollar. The market should be able to move capital between the two choices.

## The LP experience: choose how much yield to boost

This is the part I find most interesting. We do not have to make someone choose between putting their entire deposit in senior or putting all of it in junior.

A user could deposit $10,000 and leave it entirely in senior. Or they could put 10%, 30%, 47% or any other supported share into junior. The rest stays senior. They can set the level of lending-loss exposure they are comfortable with for that deposit.

In the interface, that can be one allocation slider. As the user moves it, we show their senior and junior amounts, the estimated combined yield, and how much of their deposit would take lending losses first. The extra income comes from accepting that first-loss exposure.

Using the illustrative 4% senior and 16% junior rates above:

| Your deposit in junior | Your deposit in senior | Combined interest APR |
| --- | --- | --- |
| 0% | 100% | 4.0% |
| 10% | 90% | 5.2% |
| 30% | 70% | 7.6% |
| 50% | 50% | 10.0% |

{{YIELD_IMAGE}}

These examples hold the quoted tranche rates fixed and assume the user's position is small relative to the pool. Actual quotes change with borrowing, collections and the pool's senior/junior mix. The chart uses APR without compounding; a displayed APY would need an explicit reinvestment assumption.

### A possible allocation screen

{{MOCKUP_EMBED}}

The mockup is interactive. Moving the slider changes the allocation and the estimated income at the illustrative rates. It does not place a deposit or quote a live market.

The default can be 100% senior. A user who wants more income chooses how much to put in junior. At 30%, $3,000 of a $10,000 deposit has junior exposure. That is not a $3,000 maximum loss for the whole position: the senior portion can also lose value if the pool's junior buffer is exhausted, and both portions retain their underlying market exposure.

## Moving back to senior

Putting capital into junior creates a commitment. A user should not be able to receive the junior return and switch out immediately before an already identifiable loss lands.

The working design uses a notice period to move from junior back to senior. During notice, that allocation keeps earning junior income and remains exposed to junior losses. When the notice ends, its remaining value converts to senior. That change does not need cash to leave the pool.

Withdrawing tokens is a separate action and still depends on available liquidity. The interface should show the conversion date and the cash available to withdraw separately. We have not chosen the notice duration yet.

The controller can account for the exit notice when measuring future junior commitment. That gives the market time to respond before the protection leaves. Income already earned belongs to the LP who bore the exposure, subject to collection; a new deposit immediately before repayment should not capture it.

## Do we need new LP tokens?

We have not decided whether a user locks part of a position, deposits yLP into a junior vault and receives jLP, or holds separate senior and junior receipt tokens.

My preference is to keep the user-facing choice simple: Senior and Junior, with an allocation percentage. The token design should follow the accounting.

If ordinary yLP becomes the senior claim, keeping the yLP name may be enough. If yLP remains an underlying pool share wrapped into either tranche, explicit senior and junior receipts such as sYLP and jYLP would be clearer. Those names are placeholders. We should settle how losses, accrued income and conversions attach to each claim before choosing them.

The same allocation idea should be explored for both two-sided and one-sided LP experiences. One-sided hLP already has funding debt and its own exposure mechanics, so we need to check how the tranches fit that accounting before presenting it as the same product.

## What we need to prove next

The goal is to support useful borrowing with less idle capital while offering LPs a clear choice about lending risk. The next model should test whether junior income attracts enough committed capital, what happens when junior wants to leave during a downturn, and how much borrowing remains available at competitive rates.

We should compare this with current V2 using the same total capital, including the insurance reserve. Lower losses caused only by lending less would not establish better capital efficiency.

The immediate decisions are the coverage target, the notice period, how any remaining insurance reserve fits the loss order, and whether the LP allocation is represented by a locked balance or separate receipts. The token names can follow those decisions.

<toggle>
Context and references

This is a proposal for discussion, not a description of deployed behavior. The numerical examples are illustrative.

Related Omnipair context: <mention url="https://app.notion.com/p/3197d98a444a80a7a8c7dc14dc700b98">V2 Specification</mention> and <mention url="https://app.notion.com/p/3c87d98a444a80b2ac7ed425345bfa91">Omnipair Dusk V2: Spot Margin and Leveraged LP</mention>.

[Monolith's interest-rate controller](https://docs.monolith.market/protocol/interest-rate-controller) is a useful reference for a rate that rises, holds or falls around a target band. Monolith measures redeemable debt participation. Our proposed signal is junior underwriting relative to covered debt.
</toggle>
