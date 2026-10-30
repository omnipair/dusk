# Coupled GAMM model and exit-loan checks

> Archived research, 27 October 2026. Statements below about authorization and the unmerged baseline describe that date. PR #45 merged on 29 October; see [the current plan](OP_GAMM_PLAN.md) for implementation status. Numerical results retain their original assumptions and limitations.

27 October 2026. **Mathematical specification and numerical checks only. No implementation, branch, commit, merge or PR.** Dusk integration remains deferred until #45 merges and is separately authorized.

**Recommendation:** one GAMM family can include an oracleless λ = 0 mode and an oracle-enabled λ > 0 mode. A separate pool is not mathematically necessary. Every market supports governed oracle activation later without moving its LP inventory. Activation changes the risk contract and requires a defined state transition; changing a number alone is insufficient.

The derivation below establishes the continuous, solvent model and its clean native limit. It specifies the additional equations for accrued-interest backing, but does not establish a bounded integer solver for every Dusk state. The numerical checks are synthetic mathematical cases, not native instruction tests or historical loss estimates.

Source baseline: Dusk commit `77877d576018fd2bd8191d13f05534b0484e2282`, inspected locally. The relevant definitions are [borrowing admission](https://github.com/omnipair/dusk/blob/77877d576018fd2bd8191d13f05534b0484e2282/programs/dusk/src/transitions/lending/mod.rs), [integrated hLP ownership](https://github.com/omnipair/dusk/blob/77877d576018fd2bd8191d13f05534b0484e2282/programs/dusk/src/transitions/liquidity/hlp/integrated.rs), [reserve accounting](https://github.com/omnipair/dusk/blob/77877d576018fd2bd8191d13f05534b0484e2282/programs/dusk/src/transitions/ledger.rs), and [native concentration](https://github.com/omnipair/dusk/blob/77877d576018fd2bd8191d13f05534b0484e2282/programs/dusk/src/transitions/amm/curve.rs).

## 1. One swap family including zero

Let x and y be ordinary trading coordinates in base and quote tokens, after the existing hLP/receivable decomposition. They are not necessarily available vault cash. Let C be the frozen native concentration center in quote per base, O the accepted oracle ratio in the same units, and θ the frozen concentration shape.

Write the native full-range tail and bands in normalized square-root-price coordinates u. For a band j, its lower/upper bounds are a_j and b_j and its liquidity relative to the tail is w_j:

```text
X(u) = 1/u + sum_j w_j × X_j(u)
Y(u) = u   + sum_j w_j × Y_j(u)

X_j(u) = 1/a_j − 1/b_j      if u <= a_j
         1/u   − 1/b_j      if a_j < u < b_j
         0                 if u >= b_j

Y_j(u) = 0                 if u <= a_j
         u − a_j           if a_j < u < b_j
         b_j − a_j         if u >= b_j

x = L × X(u) / sqrt(C)
y = L × sqrt(C) × Y(u)
```

The full-range tail makes X positive and decreasing and Y positive and increasing. Consequently r = y/(C x) = Y(u)/X(u) has a unique u. The native marginal price is P0 = C u². This describes the same homothetic tail-and-bands geometry; the production mapping from Dusk's amplification settings to w_j must be retained exactly.

Define the candidate price family, with 0 <= λ < 1:

```text
Pλ(x,y; O,C,θ) = C × (O/C)^λ × u(y/(C x); θ)^[2(1−λ)]
               = O^λ × P0^(1−λ)
```

At λ = 0, evaluate P0 directly. O need not exist; do not evaluate a missing logarithm and multiply it by zero. At fixed geometry, inventory sensitivity stays positive and log-price sensitivity to O is λ. Normalizing O and P0 in the same units preserves denomination consistency: changing quote units scales y, C, O and P together.

The [OP-CFMM paper](https://arxiv.org/html/2609.33799v1) provides the geometric family and conditional-invariant construction. It does not select a universal λ, guarantee oracle correctness or specify Dusk credit/hLP accounting. Dusk's band boundaries make this a piecewise-smooth extension; claiming every differentiability assumption of the paper globally would require additional work.

### Trading amounts follow an invariant, not a multiplied old quote

With r = y/(Cx) and pλ(r) = Pλ/C, use:

```text
Gλ(r) = integral from 1 to r of dq / [q + pλ(q)]
Uλ(x,y) = log(x) + Gλ(y/(Cx))
```

During a fee-free trade with O, λ, C and θ frozen, Uλ stays constant. Differentiating gives dy/dx = −Pλ. Thus a base input t determines y' from Uλ(x+t,y') = Uλ(x,y), and output is y−y'. Fees, transfer costs, actual cash availability and hLP settlement constrain the executed amounts separately.

At λ = 0, the identity Y'(u) = −u² X'(u) gives:

```text
U0 = log(x) + log[X(1)/X(u)]
```

Its level sets are precisely the native tail-and-bands curves above. Do not reuse old fixed reserve-boundary coordinates for the new invariant: boundaries must be selected consistently through the normalized native shape. An OP trajectory generally leaves the old native invariant.

For the plain CPMM case this simplifies to:

```text
λ = 0:  xy = constant
λ > 0:  (O x)^λ + y^λ = constant

buy c base, λ > 0:
cost = [y^λ + O^λ × (x^λ − (x−c)^λ)]^(1/λ) − y

sell c base, λ > 0:
proceeds = y − [y^λ − O^λ × ((x+c)^λ − x^λ)]^(1/λ)
```

The sell expression requires a nonnegative bracket. Real output/cash checks remain binding.

**Boundary finding:** the geometric blend does not preserve the CPMM's infinite-volume reserve-exhaustion boundary. At a balanced CPMM state with O = y/x, selling x × [2^(1/λ)−1] base exhausts the quote coordinate in the real-number model. This is 3x at λ = 0.5 and about 1.1601x at λ = 0.9; at zero it is infinite. A positive inventory floor or a family that retains the native tail is a design choice still to resolve. No production curve is selected by these calculations.

**27 October follow-up:** the [bounded-adjustment study](OP_GAMM_BOUNDED_ADJUSTMENT.md) tests capping the logarithmic oracle correction before integration. It restores the continuous non-depletion boundary and preserves the original blend wherever the cap is inactive. It also measures the tracking/depth cost, preserves the complete projected-health admission check, and tests clean hLP endpoints. The original geometric derivation below remains the control; the cap's numerical contract and full-state hLP solver remain open.

An oracle or λ update changes the conditional invariant used for subsequent trades. Its new level is evaluated on existing balances. The difference between old and new invariant numbers is not profit, a deposit or newly mintable LP value.

## 2. Borrowing follows the same market and counts existing exposure

Three questions must be distinguished:

1. Can buying collateral from this pool and borrowing against it return more cash than the purchase cost?
2. Can borrowing pay more than selling the same collateral, including when many positions split the exposure?
3. Can the borrower profit because the entire market overvalues collateral relative to an outside venue?

The first two concern additional extraction through lending. The third can already exist through spot arbitrage. A positive borrower profit is not, by itself, evidence that credit adds an extraction opportunity beyond the AMM.

### Native-compatible admission operator

Observe the active Pλ for each collateral direction. Retain native admission filtering and depth/exposure semantics: worsening observations can tighten immediately; a favorable observation must not reset the filter. Use the collateral-side reciprocal separately, with its own directional history.

Let Pa be the filtered admission price and Lt the recognized full-range tail liquidity. The existing borrowing shadow is a CPMM:

```text
xs = Lt / sqrt(Pa)
ys = Lt × sqrt(Pa)
V(c) = ys × c / (xs + c)
```

Concentrated bands and synthetic hLP backing are not automatically additional underwriting liquidity. External depth, when enabled, remains a separate interest-pricing input in this design.

The continuous version of current Dusk admission can be written:

```text
D_eff = effective existing debt from native aggregate-health accounting
u = V_inverse(D_eff)
W = V(u+c) − D_eff
new debt cap = (1−buffer) × min(max_CF × V(c), W)
```

This is additionally constrained by global health, actual cash, daily capacity, existing position debt and token-transfer treatment. In the inspected code, max_CF = 0.85 and buffer = 0.05, giving an 0.8075 upper factor when exposure does not bind. These constants do not imply every position can borrow 80.75% of its collateral mark.

The λ = 0 branch uses native observations and this same admission operator. A raw O never independently replaces Pa. At positive λ, derive the borrowing shadow from the OP market reference; verify its proceeds bound against the actual OP curve across sizes and bands. Carrying the old tail formula over is not, alone, a proof of that bound.

### Exit-loan certificate and its limits

Suppose a trader pays A to buy c collateral from Dusk. With the same frozen price inputs, a fee-free inverse trade returns A. If admissible debt is at most h times that reverse-trade value, where h < 1, then:

```text
exit profit = borrowed cash − acquisition cost <= −(1−h) × A
```

This establishes the fixed-state, same-pool result. It does not cover oracle changes, governance changes, future defaults, disappearing cash or buying the collateral elsewhere.

For the stronger claim that borrowing never pays more than an immediate sale, the comparison must use **net** executable sale proceeds, not a marginal price or gross shadow value. High swap fees can make that a different requirement. A further net-exit bound must not be described as existing native behavior without testing it. If it binds at λ = 0, adding it changes admission and is a separate product decision; exact native equivalence and a stronger new guarantee cannot simply both be asserted.

Aggregate debt must consume aggregate recovery capacity. Quoting each position against the same unused depth allows repeated lending to avoid the price impact a sequence of actual sales would pay. Retain Dusk's native global-health operator; the simple aggregate curves in the checks below are comparators, not replacements for it.

Public borrowing itself exchanges reserve cash for a debt claim:

```text
cash_quote' = cash_quote − draw
public_debt_quote' = public_debt_quote + draw
```

The backing coordinate does not increase, and the draw alone does not move the swap price. Posted collateral remains in its position custody. Lending does not create additional executable cash.

## 3. hLP remains coupled through ownership and cash accounting

### Clean, fully hedged state

Let N be ordinary yLP shares, EB the base-target hLP's base equity, and EQ the quote-target hLP's quote equity. Ignore fees, accrued funding, unpaid public interest and frozen cohorts for this subsection. With ordinary coordinates x,y:

```text
base_hLP_quote_debt = EB × y/x
quote_hLP_base_debt = EQ × x/y

live_base  = x + EB + quote_hLP_base_debt
live_quote = y + EQ + base_hLP_quote_debt

base_hLP_yLP_shares  = N × EB/x
quote_hLP_yLP_shares = N × EQ/y
total_yLP_shares = N + both vault-owned balances
```

These are ownership equations, so the reserve ratio in them is not a substitute for the OP marginal price. Direct substitution proves that each vault's pro-rata opposite-token claim equals its debt, and its target-token claim equals its target equity.

An OP swap selects the new ordinary endpoint x',y' from Uλ. Reconstruct both vaults using the same equations at that endpoint. Ordinary shares N and the two target equities remain fixed during the clean fee-free trade. Physical cash changes only by actual trader transfers; changes in internal hLP funding explain the difference between cash and live-reserve changes. The equations work at λ = 0 as well as positive λ.

An oracle-only update leaves x,y, all shares and both debt quantities unchanged. A matched hedge therefore stays matched. Funding accrual creates a mismatch; a new oracle price does not pay it.

For deposits, a proportional yLP deposit of fraction μ of total backing mints μ times total yLP supply and adds the corresponding actual tokens. Existing claims are unchanged; a new oracle mark is not a minting credit. A clean base-hLP deposit e adds e to EB and physical base cash, adds e×y/x to its internal funding leg and mints N×e/x vault-owned yLP shares. Ordinary x,y and the swap price stay unchanged. Admission/cash limits still apply. Single-sided conversions, withdrawals and fees need their explicit native settlement paths.

### Worked hLP swap

Start with x = 1,000 base, y = 100,000 quote, EB = 100 base, EQ = 10,000 quote, N = 1,000. Set O = 100 and λ = 0.5. A trader buys 100 base for 10,526.680780 quote.

| Quantity | Before | After |
| --- | ---: | ---: |
| Ordinary base / quote | 1,000 / 100,000 | 900 / 110,526.680780 |
| Physical base / quote, no public debt | 1,100 / 110,000 | 1,000 / 120,526.680780 |
| Base-hLP quote debt | 10,000 | 12,280.742309 |
| Quote-hLP base debt | 100 | 81.428303 |
| Target equities | 100 base / 10,000 quote | 100 base / 10,000 quote |

Vault-owned yLP shares change from 100/100 to 111.111111/90.475892. Their opposite-token net exposures remain zero. The extra live-reserve movement is matched internal funding, not trader cash or free additional liquidity.

### Accrued funding and receivables

For actual live backing RB,RQ, vault fractions a,b and indexed funding debts dQ,dB, marked equities at the common active market mark P* are:

```text
EB_mark = a RB + (a RQ − dQ)/P*
EQ_mark = b RQ + (b RB − dB) × P*
```

Example: a base hLP owns 100 base plus a 10,000 quote claim, but owes 10,100 quote. Its marked equity is 99 base at P* = 100 and 99.2 base at P* = 125. Neither revaluation repays the 100 quote gap or credits an extra 0.2 base to custody.

Keep the actual native per-token ledger identity:

```text
live reserve = cash + public cash-backed debt + flash receivable + hLP live leg
pricing reserve = live reserve − unadvanced public unpaid interest
```

Indexed hLP funding debt and its live reserve leg are distinct when interest is pending. Previously funded yield and protocol escrow remain segregated. Receivable repayment, advances and writeoff retain their existing ledger effects.

For a fully settled positive-equity endpoint, use ordinary **book** coordinates A,B rather than cash-only x,y in the ownership equations above. Reconstruct shares/debts, subtract excluded unpaid interest, and apply the native canonical curve-coordinate extraction. Denote that complete reconstruction by Ψ(state). The amount-out constraint is:

```text
Uλ(Ψ(state_after)) = Uλ(Ψ(state_before))
```

All cash, ownership, debt, fee and funding receipts must reconcile with that same endpoint. Reference/policy revisions remain frozen for the obligation. This specifies the coupled implicit problem; it does not prove uniqueness, integer rounding or affordable computation for every receivable/frozen-vault state. Copying a native quote and substituting an oracle only during recovery does not solve it.

The clean λ = 0 limit is verified below. Full native equivalence also requires preserving Ψ, funded-yield checkpoints, fees, loss treatment, cohort rotation and controller updates in actual integer execution. No negative equity is turned into executable liquidity to force a solution.

## 4. Numerical attack checks

These checks use analytical formulas, 60-digit decimal arithmetic and independent numerical integration of dy/dx = −Pλ. Calculations ran in memory; no executable source files were created. There are no real borrowers or simulated historical oracle failures in these examples.

Unless specified otherwise: ordinary reserves x = 10,000 base and y = 600,000 quote; true outside price 60 quote/base; no fees, interest or active hLP in the credit-attack cases. The illustrative debt factor h = 0.8 is not a selected Dusk parameter. Admission in these cases is 0.8 times the smaller of the full-size OP sale value and a tail shadow value calibrated to the active OP price. Filters are assumed settled except in the explicit activation comparison. Runtime global-health, daily limits and transaction admission are not replayed.

### Same-pool purchase and exit

With a correct, fixed O = 60, buy 3,000 base and attempt to borrow against it:

| λ | Purchase cost | Profit with naive spot-only collateral valuation | Profit with size-aware coupled valuation |
| --- | ---: | ---: | ---: |
| 0 | 257,142.86 | +36,734.69 | −51,428.57 |
| 0.25 | 232,462.19 | +8,084.24 | −59,835.02 |
| 0.5 | 212,015.94 | −11,790.10 | −65,163.56 |
| 0.75 | 194,767.22 | −25,874.80 | −68,483.56 |
| 0.9 | 185,645.49 | −32,339.11 | −69,795.73 |

The naive control is intentionally defective and is not Dusk's current admission. The coupled result agrees with the fixed-state inverse-trade bound.

### Incorrect common oracle

Let the outside price remain 60 while O incorrectly reports 100 long enough to pass the chosen filters. Buying 100 base outside costs 6,000:

| λ | Coupled loan | Borrow-and-abandon profit | Direct sale to OP pool | Loan minus direct-sale proceeds |
| --- | ---: | ---: | ---: | ---: |
| 0 | 4,752.48 | −1,247.52 | 5,940.59 | −1,188.12 |
| 0.25 | 5,396.33 | −603.67 | 6,763.12 | −1,366.79 |
| 0.5 | 6,127.16 | +127.16 | 7,701.82 | −1,574.67 |
| 0.75 | 6,956.64 | +956.64 | 8,774.02 | −1,817.39 |
| 0.9 | 7,507.13 | +1,507.13 | 9,489.72 | −1,982.59 |

Borrowing can be profitable, but selling pays more for each displayed size. This demonstrates common oracle-error risk, not an extra single-position lending advantage. In the infinitesimal-size case, with initial native price equal to true price S, positive loan profit requires h×(O/S)^λ > 1. With h = 0.8 and O/S = 100/60, the threshold is λ > 0.436829. This is a conditional threshold, not a recommended λ ceiling.

A sensitivity using the inspected native low-exposure upper factor 0.8075 raises the λ = 0.5 example loan to 6,184.60. That is still a simplified upper-envelope calculation, not a native admitted-position result.

A size search over 1–3,000 base in integer steps found maximum borrower profits of 139.71, 3,435.39 and 7,519.18 for λ = 0.5, 0.75 and 0.9. A closed-form shadow-curve optimum independently gives first sizes about 143.1545, 687.5678 and 997.9151 base. Neither tested zero nor 0.25 had a profitable size under this particular oracle error; larger errors can change that result.

### Repeated loans must consume exposure

As an intentionally broken control, omit aggregate exposure and issue 80 loans against 100 base each while the λ = 0.5 market still observes the wrong O = 100. Because borrowing exchanges cash for receivables, the price coordinate does not move:

```text
total loan cash                       490,172.43
outside acquisition cost              480,000.00
borrower profit                        10,172.43
one full-size OP sale of 8,000 base    412,549.20
extra exit cash versus that sale       77,623.23
80% of aggregate tail recovery        259,690.01
```

The last number is an illustrative aggregate-cap comparator. Dusk already has global-health and effective-existing-debt accounting; this result is evidence to preserve and test it, not a verified Dusk exploit. It also shows why per-position price agreement is insufficient.

For a separate loss stress, execute ten profitable loan/default cycles. After each default, sell collateral outside at 60, write off only the actual shortfall, restore the honest oracle to 60 and let fee-free arbitrage clear the pool before the next bad-feed interval. Optimize collateral size each cycle and carry balances forward:

| λ | Borrower profit over ten cycles | Total LP loss | Included recovery-arbitrage loss |
| --- | ---: | ---: | ---: |
| 0 / 0.25 | 0 | 0 | 0 |
| 0.5 | 1,396.36 | 1,396.40 | 0.04 |
| 0.75 | 33,914.50 | 33,926.67 | 12.17 |
| 0.9 | 73,106.00 | 73,129.05 | 23.05 |

These are deliberately favorable conditions for the attacker: faulty accepted reports, no acquisition/transaction fees, unlimited outside execution at 60, immediate modeled default recovery and sufficient time between faults for filters/limits. They do not estimate the cost or probability of compromising a provider. Credit is not asserted to dominate a repeated spot-arbitrage strategy. The ledger check is LP loss = borrower gain + recovery arbitrage in this no-insurance, no-fee construction.

### Activation and fee controls

In the same wrong-feed fixture, buying 100 base under λ = 0 costs 6,060.61. Immediately switching to λ = 0.5 and replacing admission history permits a modeled 6,188.70 loan, producing 128.09 profit. Retaining the old admission bound limits the modeled draw to 4,848.48. Warm-up blocks this instantaneous path; it does not fix a persistently false reference. A correct new reference can create legitimate repricing arbitrage without creating bad debt, so proposal profit alone must not be mislabeled an exploit.

A separate gross-versus-net control keeps λ = 0 and h = 0.8. Gross collateral-sale value for 100 base is 5,940.59, so the candidate loan is 4,752.48. With a hypothetical 50% input swap fee, direct net sale pays only 2,985.07. That creates a 1,767.40 loan-versus-sale difference, although both are below the 6,000 outside acquisition cost. This is a counterexample to a blanket net-sale theorem, not a profitable external purchase attack or a native runtime finding. The native dynamic-fee and actual admission combination requires its own test before changing policy.

### Independent mathematical checks

| Check | Result |
| --- | --- |
| 70 CPMM-family cases: five λ values, two references, seven purchase fractions from 0.01% to 90% | Closed form versus independent price-ODE integration: maximum relative error 9.89×10^-15; reverse-path error 7.18×10^-15. |
| Nine two-band cases: λ = 0, 0.5, 0.9; 10%, 30%, 60% purchases | λ = 0 endpoint agrees with independently reconstructed native geometry within 2.63×10^-10 relative error. Maximum reverse-path error 9.09×10^-15. |
| 30 clean hLP endpoints across five weights, both directions and three sizes | Maximum 60-digit claim residual 2×10^-55; ledger residual 4.5×10^-55. These are real-number calculations, not atom-rounding tests. |
| Quote-denomination change by 1,000 | Concentrated native-basis price scales by 1,000 under the normalized construction. |
| λ = 0 semantics | All formula evaluations use the native branch without consulting O. Actual account-validation and compute equivalence remain untested. |

The two-band check uses center 100, core price bounds 100/1.1 to 100×1.1, shoulder bounds 100/1.5 to 100×1.5, and band weights 1.5 each relative to the unit tail. This is a four-times-active-tail fixture, not a claim of Dusk's configured 4x peak-amplification setting. Independent integration used fourth-order Runge–Kutta with 8,000 steps per leg; the concentrated native endpoint was recovered separately from the original band inventory formula. The λ = 0.5 loan was also recomputed using square roots and the shadow-curve expression directly.

## 5. Governed activation in one market

Use one market family with governed oracle activation available to every market. Creation defaults to λ = 0 or accepts an optional bounded oracle policy in that same instruction. Subsequent activation and changes use the existing proposal authority. The 31 October policy removes creation-time activation bounds, permanent oracleless profiles and the separate initial-oracle setter; this changes configuration semantics, not the dated mathematical experiments above. No Horus permission or privileged provider is needed.

| State | Price-feed dependency | Required behavior |
| --- | --- | --- |
| λ = 0 | None | Native pricing and coupled native admission; no price account, refresh or freshness requirement. Optional depth-fed interest is a separate dependency if explicitly enabled. |
| Activation pending | Current λ = 0 actions remain oracle-independent | Bind proposed provider/feed/instrument, λ bounds and activation terms. Candidate feed observations may build history but may not revalue live loans early. |
| λ > 0 | Configured accepted observation | One account-read contract, with market-specific validity and outage policy. Coupled pricing, admission and hLP references follow the active revision. |
| Proposed return to zero | Governed transition | Preserve balances, debt, yield, loss history and admission safeguards; do not wipe favorable old EMA history into instant extra capacity. Do not automatically return to zero because a feed is stale. |

Activation must perform these five operations as one specified transition:

1. Validate the approved oracle identity, permissions, revision, quality/history and proposed λ. Disclose provider upgrades and governance authority before deposits.
2. Accrue elapsed debt/funding under the old settings and checkpoint existing claims. Complete or reject conflicting flash obligations; proposals cannot replace their fixed payment/reference midway.
3. Preserve all physical balances, debt principal/shares and LP ownership. Install the new conditional pricing function at the same inventory; do not mint shares or clear a deficit because its invariant number changed.
4. Switch swap, risk and hLP reference semantics coherently. Preserve directional protection and invalidate old prepared quotes, controller targets and caches. Test both parameter increases and decreases against existing positions; stored position terms are not silently rewritten.
5. Enforce the approved bounds and report the new revision. A timelock or gradual λ ramp alone does not prove fair execution or guarantee that everyone had cash available to exit.

The layout and policy capability must exist first. Adding it to a program that lacks it still requires a program/account-version change; after that, an allowed λ proposal need not migrate LP inventory. There are no production accounts to migrate in the present repo context.

The remaining implementation inputs are specific: preserve native integer behavior at zero; solve and bound the full Ψ endpoint with receivables/frozen hLP; decide geometric-tail boundary handling; define the desired gross/net exit guarantee with existing fees; and validate activation on actual admitted debt states. The mathematical evidence supports a unified design, not approval to deploy it.
