# Margin and flash liquidation decisions

Updated 2026-10-11. This is the current decision record for implementation in
[PR #45](https://github.com/omnipair/dusk/pull/45). It supersedes conflicting
pricing, eligibility, reward, clock and composability proposals in the earlier
handoff, flash contract and calibration documents. The implementation checkpoint below distinguishes current code and tests from
historical calibration evidence.

## Leverage funding simplification, 2026-10-11

The user reversed optional collateral-funded leverage from commit `65a948fd`.
Openings accept only the debt asset: USDC for a META long, META for a META short.
Margin plus borrowing is swapped into the opposite asset held in custody.
A UI zap may convert another wallet token before the opening instruction.
The optional native entry/close instructions, funding marker, extra event fields,
and dedicated SDK close solver are removed. This supersedes the earlier #48
feature-retention decision. Audit remedies, stored margins and flash liquidations
remain part of #45.

The requested next discussion covers size-only adjustments, debt-repayment margin
increases, removing standalone margin withdrawal, combined add/remove actions,
and user-selected leverage below the permitted maximum. This funding reversal
does not change the selected IM/MM formulas or implement that next action redesign.

## Latest design steering, 2026-10-10

This section takes precedence over earlier conflicting proposals below. The
user accepted combining health and elapsed-time incentives, partial-first AMM
recovery and target-based insurance funding, and requested a small insurance
number analysis. These design choices are authorized for implementation; the
unselected numeric examples below are not runtime defaults.

- **Timing:** the user approved `min(cap, max(health_discount, time_discount))`
  for the ordinary flash-purchase incentive. This increases the incentive when an
  eligible position remains unfilled, with profitable execution intended from
  the beginning. This supersedes the earlier ban on distress clocks as pricing
  inputs. It does not approve a mandatory wait before liquidation, an expiry,
  or time alone authorizing below-floor losses. Preserve the selected 0.5–5%
  health candidate as a component. The user selected **120 seconds to reach
  the 5% cap**. Use linear interpolation from 0.5% to 5% as the implementation
  starting point; the greater health discount can apply sooner. Persistent
  observations and recovery resets are implemented and tested. Start from a committed on-chain eligibility
  observation, not state created only inside a reverted flash transaction.
  Useful partial fills do not reset continuing distress; verified recovery ends
  the episode. Prevent stale episode reuse after recovery or position reopening.
- **Emergency sizing:** the user accepted partial-first AMM recovery after
  challenging automatic full liquidation. Develop partial-first sizing after actual costs;
  distinguish failure to reach the entire recovery target from failure to make
  useful progress. The user subsequently approved full liquidation within the
  emergency boundary when symmetric-EMA equity is zero or negative, or when
  the program verifies that no permitted partial liquidation improves health
  after all costs. Failure to reach the entire MM+2-point target is not enough:
  retain useful partial recovery. The infeasibility check must evaluate the
  protocol-permitted sizes, not infer impossibility from a keeper's chosen size,
  restrictive request or failed transaction. The user selected per-market
  minimum residual debt and no fixed insurance-funded cleanup bonus. Express
  the minimum separately in each debt token's raw units; actual per-market
  amounts still need configuration (5 USDC was illustrative, not a universal
  default). Resize partials to prevent avoidable subminimum leftovers while
  preserving health improvement. The dust rule must not manufacture full-close
  permission by excluding otherwise useful partials. A poor spot quote alone
  must not become a new permission to liquidate a healthy EMA position.
  The user selected emergency access at **70% of applicable maintenance**:
  at 7% MM, permit access at or below 4.9% symmetric-EMA equity. This selects
  access, not automatic full closure or evidence that execution is solvent.
  The user subsequently approved a progressive emergency caller reward capped
  at **1% of actual internal sale proceeds**, including insolvent sales. Use
  health deterioration or elapsed distress to increase the reward; exact curve
  interpolation must be specified and tested. This is separate from ordinary
  flash buyers' purchased-collateral upside. Keep percentage-based cleanup
  incentives without a fixed insurance-funded bonus; a capped percentage is
  not a guaranteed minimum reward or guarantee of profitable dust cleanup.
- **Insurance:** the user accepted reducing future top-ups as a fund reaches
  a target, with more designated fee income going to LPs, and requested a small
  numerical study. Funding contributions and draw limits are separate policies.
  Following the target sensitivity analysis, the user selected **5% of
  outstanding covered borrower/leverage principal per debt token** as the
  provisional calibration target. The user selected an additional **one-fifteenth
  of current indexed hLP funding debt (approximately 6.67%)** in the same debt
  token. The combined target is `ceil(P / 20) + ceil(H / 15)`, with ordinary
  borrower/leverage principal P excluding hLP funding debt H. The user approved markets
  launching with zero insurance and optional pre-funding. There is no required
  minimum seed or launch gate; actual insurance cash limits loss coverage. The user
  also selected full designated-fee funding through **75% of the target**, then
  a **linear taper to zero at 100% of the target**.
  The selected liquidation charge is now 1% of net debt repaid. The user
  allocated 0.8% of repayment to insurance/LPs and 0.2% to protocol. The user
  confirmed that insurance top-ups come from the 0.8% allocation through the
  target-based taper: all of that allocation funds insurance while sufficiently
  underfunded, progressively more goes to LPs near target, and all goes to LPs
  at or above target. The protocol's 0.2% remains separate. Existing insurance
  is not distributed when the target falls. If draws lower the funding ratio,
  the same curve increases future top-ups again. The 75% taper starting point
  is selected; a 4/4 insurance/LP split occurs halfway between that point and
  the target, not at the taper starting point.
  Following the user's affirmation of item 4 in the remaining-decisions list,
  carry forward **up to 75% of eligible principal losses, bounded by a shared
  50% fund budget** as the selected spending direction. The user subsequently
  confirmed preserving hLP's existing eligible-shortfall coverage, up to 100%
  of that eligible shortfall subject to the same shared fund budget. Do not
  apply the borrower/leverage 75% coverage rate to hLP or give hLP a separate
  50% budget. Coverage entitlement does not guarantee available cash.
  The user confirmed that this shared
  budget refreshes every 24 hours against the remaining insurance balance.
  It does not reset per liquidation or at a calendar-day boundary.
  This replaces the percentage-of-fund per-event cap in the candidate model;
  it is not an additional cap stacked on that existing 20% rule.
- **Protocol liquidation revenue:** the user explicitly requires a protocol
  treasury cut from solvent liquidations, in addition to normal collected
  interest and applicable Dusk swap-fee revenue. Include both borrowing and
  leverage, with the charge represented in fixed-payment quotes and partial
  recovery calculations. Full settlements must not create a shortfall to collect
  this fee, and insolvent settlement has no additional treasury liquidation fee.
  The selected total rate is **1% of net debt repaid**: **0.8% to the user's
  LP/treasury allocation and 0.2% to protocol**, equivalent to an 80/20 split
  of the collected charge. These are percentages of repayment, not additional
  charges on top of the 1%. Insurance receives top-ups from the 0.8% allocation
  under the selected taper direction, and LPs receive its remainder. The 80/20
  split applies to the actual collected fee after any solvent-surplus cap;
  neither recipient is promised an unfunded amount. The user selected the
  existing market LP yield distribution for the remainder: yLP receives its
  proportional share, including hLP through its underlying yLP holdings.
  Preserve those ownership and checkpoint rules; do not apply a second
  protocol deduction to an already allocated LP amount.
- **Fee base and collateral charge, 2026-10-10:** the user approved specifying
  the net debt repayment first and funding the liquidation fee with additional
  collateral. For a debt repayment R, calculate the bounded fee F on R and bind
  both obligations; ordinary buyer payment must fund R plus F after applicable
  transfer fees. Do not deduct F from the debt repayment promised in the quote.
  The collateral quote includes both the fee and the buyer discount, and partial
  recovery checks include all seized collateral and all actual costs. At the
  selected 1% fee, R=1,000 requires 1,010 spendable payment before token-fee
  gross-ups, with 1,000 clearing debt, 8 to LPs/treasury and 2 to protocol.
  This supersedes
  earlier examples taking 2 out of a 1,000 payment and reducing debt by 998.
  The prior 0.2% total-charge candidate is superseded. The user explicitly kept
  the ordinary 0.5%-to-5% buyer discount and progressive emergency caller reward
  capped at 1% of proceeds unchanged. The liquidation charge and emergency
  caller reward have different bases and remain distinct amounts.

The historical 75%-of-MM recommendation and its 2,304-run evidence apply to the previous
health-only incentive with full emergency sales. They do not validate this
revised partial-first/time-escalating design or validate the selected 70% threshold.
Keep the agreed margin terms,
V2 linear symmetric-EMA eligibility, atomic fixed-payment settlement, and
principal-first loss accounting while resolving these changes.

## Confirmed choices

| Topic | Selected behavior |
| --- | --- |
| Ordinary liquidation eligibility | Symmetric EMA health alone. Preserve V2's linear collateral valuation: hypothetical AMM price impact and LP withdrawal do not independently make a reference-healthy position liquidatable. Preserve the separate scheduled-transfer-fee remedy. |
| Admission valuation | Preserve directional/asymmetric EMA protections for new risk and existing cash/accounting constraints. Deliberate use of different EMA roles is not a finding by itself. |
| Initial margin | User selected effective maintenance plus 3 percentage points, with same-side aggregate crowding able to raise the requirement further: IM = max(effective MM + 3 points, crowding requirement). Apply to new or increased risk. No replacement hard cap at 50% TVL. |
| Maintenance | Progressive position-size bands scaled to a conservative liquidity reference stored at entry. LP withdrawals and other traders' entries do not rebase an existing position's bands. Risk increases recheck applicable terms and expose changes in the preview. |
| Persistence of entry terms | User accepted retaining the entry-equity requirement as predictable UX. Calculate and store the admission obligation and maintenance terms/reference at entry. For an unchanged position, other traders' activity, LP depth changes and price movements alone do not reselect its IM/MM terms. Price movements and accrued interest still change actual equity, health and the estimated liquidation price. Increasing risk rechecks applicable admission terms with a preview; reducing collateral releases the retained obligation proportionally and can lower effective MM under the stored bands. Runtime implementation and native lifecycle coverage are described in [stored leverage margins](STORED_LEVERAGE_MARGINS.md); full loss calibration of the new policy remains outstanding. |
| Calibration priority | Preserve gentle small-position requirements near today's 10% IM / 7% MM, with progressive protection for larger positions. The user found the tested 7/12/30% and 7/20/40% schedules too restrictive around 30% of the stored side reference; they are comparison candidates, not selected defaults. |
| Leverage target at 30% of the stored side reference | User selected approximately 6x. This is a reference-margin calibration target before execution costs, additional crowding or other admission constraints. It implies about 16.67% IM and, with the selected 3-percentage-point entry buffer, about 13.67% effective MM. It does not select the entire tier curve or guarantee 6x wallet leverage in a thin AMM. |
| Margin UX | Emphasize available leverage, required collateral and estimated liquidation price in the main flow. Raw IM/MM percentages are secondary information. Displayed leverage must include actual execution and crowding constraints. |
| Partial liquidation | User confirmed a recovery target of the remaining position's effective MM +2 percentage points. Calculate it using the surviving position's size and terms, after all liquidation costs. Accept smaller useful fills that improve health; each fill need not reach the whole target. Size the maximum seizure to avoid unnecessary liquidation after recovery; full liquidation for insolvency, infeasible recovery or uneconomic leftovers under explicit rules. |
| Buyer economics | Quote a specified collateral slice for a specified net debt-token payment. The buyer can route anywhere, pay from inventory, or retain purchased collateral; upside on the purchased slice belongs to the buyer. Unsold collateral remains position-owned. |
| Incentive | User approved the greater of health-based and elapsed-time discounts, bounded by a 5% cap. Preserve 0.5% at MM to 5% at zero-equity health; the user selected 120 seconds for the time component to reach 5%. Use linear 0.5%-to-5% interpolation as the implementation starting point. Keep immediate eligible fills and competitive keeper racing; a capped incentive does not guarantee profitable execution. Buyer retains purchased-slice upside. |
| Timing | Immediate eligible fills; no mandatory auction wait, expiry or time-only emergency unlock. A persistent confirmed-distress clock now increases the incentive. Useful partial fills do not reset ongoing distress; verified recovery ends it. |
| Earlier solvent AMM close | Previously approved for modeling only. The user subsequently objected that this separate full-close permission could undermine better execution and partial repair through flash liquidation, then confirmed dropping the proposed 0–2% shortcut. Dusk AMM execution remains a permitted ordinary flash route under the same bound payment and partial-fill rules, with session-bound debt netting where needed. Emergency access instead uses the selected 70%-of-MM boundary. |
| Loss-taking emergency | Permit Dusk-AMM recovery below the ordinary flash-payment floor when symmetric-EMA equity is at or below 70% of applicable MM, partial-first after costs. At 7% MM this is 4.9% equity. Within that boundary, the user approved full liquidation if symmetric-EMA equity is zero or negative, or the program verifies that no permitted partial improves health after all costs. A partial need not reach the whole MM+2-point target to take precedence. A keeper's failed or restricted request is not proof of infeasibility. Selected per-market dust minimums do not create separate full-close permission; amounts require configuration. A poor AMM quote alone does not authorize loss-taking, and an external buyer cannot reduce the bound payment after release. |
| Emergency caller | User approved a progressive reward capped at 1% of actual internal sale proceeds, including insolvent sales, increasing with deteriorating health or elapsed distress time. Use the approved health/time curve in the current implementation checkpoint below. This compensates the caller when Dusk performs the sale itself; it is not an extra payment to ordinary flash buyers. No fixed insurance-funded cleanup bonus. |
| Economic dust | User selected per-market minimum residual debt, expressed separately in each debt token's raw units, and no fixed insurance-funded bonus. Resize partial fills to avoid subminimum leftovers while still improving health. Dust alone does not authorize a full close or loss-taking; do not manufacture infeasibility solely through the dust rule. Per-market amounts require configuration; the earlier 5-USDC illustration is not a universal default. Percentage rewards do not guarantee profitable cleanup of naturally tiny positions. |
| Loss waterfall | Full-liquidation recovery protects principal first. Insurance covers remaining principal only within applicable limits. Remaining principal is written off, unpaid interest is canceled, and canceled interest generates no protocol/referral fees. Finish with zero debt on the fully settled leg. |
| Insurance spending | Up to 75% of eligible borrower/leverage principal shortfall. Preserve hLP's existing eligible-shortfall coverage, up to 100% of that shortfall. All claims use the same debt-token fund's shared 50% budget, refreshing every 24 hours against remaining available insurance; hLP does not receive a separate budget. Credit inflows and previous draws affect the current window budget. Gross vault spending and net coverage differ for transfer-fee tokens. Preserve hLP eligibility and residual-loss accounting; its coverage rate does not expand eligible borrower/leverage losses to unpaid interest. |
| Protocol liquidation revenue | Charge 1% of net debt repaid on eligible solvent borrowing and leverage liquidations: 0.8% of repayment to insurance/LPs and 0.2% to protocol (80/20 of the collected fee). Fund through additional collateral; never reduce the quoted debt repayment by the fee. Include it in partial-health calculations and cap full-settlement collection by actual surplus after repayment and applicable caller reward. No additional treasury liquidation charge on insolvent settlement. Distribute the LP remainder through existing market LP yield ownership, including hLP through its yLP holdings, without another protocol deduction. Normal interest/swap revenue remains governed by its existing accounting. |
| Insurance funding | The user selected the combined target `ceil(P / 20) + ceil(H / 15)` per debt token: 5% of outstanding borrower/leverage principal P, plus one-fifteenth (approximately 6.67%) of that token's current indexed hLP funding debt H. P excludes H; H includes accrued funding interest. Markets may launch with zero insurance; pre-funding is optional, with no required seed or launch gate. All of the 0.8% insurance/LP allocation goes to insurance through 75% of the combined target; its insurance share then decreases linearly to zero at 100% of target. Existing market LP yield recipients receive the remainder, including all of the allocation at or above target; hLP participates through its underlying yLP holdings. Lower balances after draws increase future funding under the same curve. The 0.2% protocol allocation is separate. Do not distribute existing insurance when the target falls or create a full-settlement shortfall to collect fees. The 75%-loss / shared 50%-fund / 24-hour draw direction remains selected. |
| Composability | Lock the liquidated position and its bound obligation. Allow other-position swaps, borrowing, leverage and independent liquidations. Also allow LP withdrawals and relevant parameter changes with additional accounting checks; do not introduce a blanket same-market ban. |
| Ordinary leverage execution | Preserve ordinary Dusk AMM execution. General external owner open/close routing is outside the agreed implementation scope. |
| PR coordination | Keep consolidated work in #45. The 2026-10-11 funding simplification supersedes retaining #48's optional collateral-funded feature; preserve unrelated audit remedies. |

## Settlement invariant

```text
begin: confirm eligibility; bind collateral X, net debt repayment R and fee F
       keep original debt/exposure/reservations live; activate position session
       release X collateral
buyer: route freely or supply its own funds, retaining its purchased-slice upside
settle: verify the original session and actual net debt-repayment credit >= R
        separately verify/distribute fee F; fee payment cannot satisfy R
        apply the bound debt reduction and fee allocation
        partial: retain unsold collateral and remaining debt, update health
        full: recovery -> eligible insurance -> write-off/cancel -> clear debt leg
```

All endpoints, transfers and final loss accounting are atomic. Transfer-fee tokens
require measuring spendable credits at every physical leg, including the final
session-vault-to-reserve transfer. Existing funds or an unrelated donation must
not satisfy a new obligation. Never debit the owner's wallet as an implicit
source of repayment.

The session binds the original position identity, debt shares, principal,
authorized collateral and payment. A borrower moving liabilities, reopening an
account, or transiently setting debt/collateral to zero cannot erase it. Normal
operations elsewhere still enforce all their own health and cash rules. Released
custody cannot also count as available liquidity or unencumbered collateral.
Shared-market operations must preserve each active session's reservation and
settlement contract, including when parameters or insurance availability change.

Full settlement with an insolvent fixed-price collateral purchase needs a
protocol-authorized payment and loss calculation before release. A buyer's bad
route never authorizes an additional insurance claim. Internal emergency pricing
still carries manipulation risk; atomicity alone does not close finding #287652.

The historical additional solvent AMM candidate measures executable repayment after the bounded
caller reward and insurance contribution. It requires principal plus interest to
be fully funded from actual sale recovery. A quote is a preview, not permission
to draw insurance if final execution falls short; that transaction must revert.
The solvent contribution is capped by post-repayment surplus. Do not double-count
execution fees already included in the quote or accept caller-defined expenses.
Keeper network costs come from the keeper's reward. The owner receives residual.

This mode can allow a full closure before the critical-EMA loss mode, but only
after ordinary EMA liquidation eligibility. Price gaps, liquidity loss and delayed
keepers can skip the solvent window. The separate loss mode remains necessary.
Manipulating the AMM quote can also change full-close permission and reduce owner
surplus even when debt is fully paid. Prefer partial repair, but do not claim that
off-chain keeper preferences enforce priority or prove outside routes unavailable.
The user raised this conflict with borrower execution quality on 2026-10-07;
the 0–2% shortcut is therefore withdrawn from the current recommendation.

### Dusk as an ordinary flash route

A buyer may sell released collateral through Dusk, an external venue, a mixed
route, or retain it and pay from inventory. All ordinary routes owe the same
bound payment. Output below that payment is not a venue ban: a buyer can supply
the difference from its own funds, otherwise settlement rolls back. A poor
route never lowers the payment floor. Fixed-payment buyers retain purchased-slice
upside, so route freedom improves access to profitable fills without guaranteeing
that the borrower receives the best execution surplus.

Preserve Dusk's internal debt-netting capability where needed. An ordinary spot
swap must fund its full output before later flash repayment; existing integrated
close/liquidation policies can offset the repaid principal and need physical cash
for residual, interest, fees and other actual outflows. A session-bound internal
swap/repayment path needs explicit design and instruction tests under the same
price, seizure and recovery terms. Never relax the ordinary swap cash guard or
pretend accounting offsets are measured token credits. Validate both debt
directions, partial repayment, transfer fees, hLP funding and shared operations
before treating same-market routing as equivalent to integrated settlement.

## Historical calibration and design rationale

### Insurance target follow-up, 2026-10-10

**hLP allowance selected by the user:** use a target per
debt token of `ceil(P / 20) + ceil(H / 15)`, where P is outstanding ordinary
borrower plus isolated-leverage principal and H is that token's current indexed
hLP funding debt, including accrued interest. P excludes hLP funding debt;
H for Base belongs to the quote-target hLP vault, and H for Quote belongs to
the base-target hLP vault. Do not add hLP NAV, both vaults' debt or another
denomination to the same token's target.

The hLP component is approximately 6.67% of H. It scales the existing 5%
borrower target by `100% / 75%` to reflect the selected coverage fractions.
This preserves a common illustrative shortfall budget of 3.33% of each
exposure under the shared 50% spending limit, assuming full funding and no
transfer fees or prior window draws. It is a budget-normalization assumption,
not evidence that borrower and hLP losses have equal frequency or severity.
Current hLP terminal eligibility only accepts shortfalls bounded by accrued
funding interest; using total indexed funding debt as a sizing proxy does not
broaden that claim eligibility.

For P=100,000 USDC and H=30,000 USDC, the principal component is 5,000 USDC,
the hLP allowance is 2,000 USDC and the combined target is 7,000 USDC. The
fully funded window budget is 3,500 USDC, shared by all eligible claimants.
The allowance is part of the funding target, without a separate hLP vault,
reservation, fee or guaranteed payout. Apply the selected 75%-to-100% taper
to the combined target. When the target falls, retain existing insurance and
adjust only future fee allocation. The user approved this starting
parameterization; its sufficiency still requires integrated stress validation.

The user requested a quick statistical check before choosing the target.
[Insurance target results](calibration/INSURANCE_TARGET_RESULTS.md) recompute
27 funding sensitivities with the selected 0.8% insurance/LP allocation and
summarize the 2,304 historical stress rows as 768 matched fixtures. The user
subsequently approved 5% of outstanding covered borrower/leverage principal
per debt token as the provisional calibration target. This does not establish
a statistical optimum. The user subsequently selected the 75% taper starting
point and linear decrease to zero insurance funding at the target. At full funding,
its 50% window budget
supports 75% coverage of shortfalls up to 3.33% of covered principal, before
competing hLP claims or transfer fees. Funding speed varies materially with
solvent liquidation volume. The user approved zero-insurance launches and
optional pre-funding; the 1,000 seed in the study is illustrative, not required.
The selected hLP component is H/15; the numerical funding table excludes hLP
claims and does not validate adequacy of that combined target.
The historical stress outcomes do not validate the latest liquidation design.

Optional launch funding uses the existing `fortify_market` donation path, which
records actual net insurance-vault credit. Market initialization already starts
with zero insurance. SDK builders `fortifyMarketInstruction` and
`fortifyMarketTransaction` now support optional top-ups without fetching the
market; callers can compose them after initialization within transaction limits.
The donor receives no LP or withdrawal claim. Focused SDK validation covers
both token programs, exact u64 encoding, custom program IDs and source accounts,
mint-owner lookup, invalid inputs and building before the market exists. The current implementation also includes the liquidation fee, target/taper and draw policy.

The selected curve applies to the insurance/LP allocation only. With target T
and fund balance B, the insurance fraction is 100% for B <= 0.75T, then
(T - B) / (0.25T) for 0.75T < B < T, and zero for B >= T. LPs receive
the remaining designated allocation; the protocol's separate 0.2% is unchanged.
Integer rounding, transfer-fee credits, observation timing and handling a
single contribution that crosses the target still need accounting-level tests.
At T=5,000, an 8-token allocation splits insurance/LP as 8/0 at B=3,750,
4/4 at B=4,375, and 0/8 at B=5,000. These figures specify allocation intent,
not a claim that the new runtime funding path is implemented.

### Insurance budget sensitivity, 2026-10-09

**2026-10-10 update:** the user selected a 1% total fee with a 0.8% LP/treasury
and 0.2% protocol split. The historical funding cases below do not model that
split. The user confirmed that the 0.8% allocation funds insurance through the
target-based taper and sends the remainder to LPs. Recalibrate funding speed
using that source; do not treat the entire 1% or the protocol's 0.2% as insurance.

The user authorized a small insurance-number study after accepting the combined
health/time incentive, partial-first AMM recovery and target-based funding.
[Insurance policy results](calibration/INSURANCE_POLICY_RESULTS.md) contain 27
funding and 72 spending cases, with conserved integer cash accounting. This is
a deterministic fee/budget model, not native simulation of the revised
liquidation design or a prediction of loss probabilities.

The proposed package for integrated testing is a 5% target against covered
borrowing/leverage principal per debt token plus a calibrated allowance for
existing hLP insurance claims and a minimum seed; full designated-fee funding
until 75% of target, then a linear taper to zero at target. The 0.2% liquidation
levy remains a secondary-funding candidate. At an assumed 100,000-USDC monthly
eligible fee base, it produces only 200 USDC/month; a 1,000 seed reaches 95% of
a 5,000 target in about 23.8 months under that taper. These are hypothetical
inputs and proposed numbers, not selected defaults or sufficiency claims.

Spending analysis supports replacing the percentage-per-position cap with a
shared loss budget for further testing. With a 5,000 fully funded vault, the
current 20% event cap covers only 1,000 of one 2,500 shortfall, but covers the
same total split into five 500 shortfalls. Removing that cap while preserving
the 50% window budget covers either form equally. A 50% shared budget still
writes off loss with insurance remaining; a 100% budget can exhaust insurance.
The latest selected spending direction covers up to 75% of each eligible principal
shortfall within that shared 50% budget. The user confirmed the 50% shared budget's
24-hour refresh against the remaining balance and affirmed the insurance direction
in the remaining-decisions list. These rules are implemented in the current checkpoint below. With 5,000
insurance and an unused 2,500 window budget, a
1,000 principal shortfall draws 750 and writes off 250; a 5,000 shortfall draws
2,500 and writes off 2,500. Repeated 1,000 shortfalls draw 750, 750, 750, 250,
then zero in the same window. LPs therefore bear at least 25% of each eligible
principal loss even before the shared cap binds. Every full settlement still
clears the debt leg; the uninsured remainder cannot be repeatedly claimed.

The 50% basis is window-opening available insurance plus credited inflows,
less budget already consumed, shared across the debt-token vault's claimants.
It is not reset separately for each position. After a complete 24-hour window,
the budget resets against the remaining balance; half the original fund is not permanently
reserved. The 75% loss target is net coverage while the vault budget constrains
gross debit, so transfer fees may lower achieved net coverage. Current program
ceilings require an implementation change to replace the per-event cap.

The hLP terminal path is a separate existing claimant against the same fund.
Its funding-interest shortfall cannot be omitted merely because borrower and
leverage insurance is principal-only. The user confirmed preserving hLP's
existing eligible-shortfall coverage, up to 100% subject to the shared 50%
draw budget. Applying 75% to hLP claims in the historical mixed fixture is
only a sensitivity and does not represent the selected policy. Neither hLP
nor another position type may reset or duplicate that shared budget. For a
5,000 fund with 2,500 unused window capacity, a 1,000 eligible borrower loss
draws 750; a subsequent 1,000 eligible hLP shortfall draws 1,000, leaving 750
window capacity. Reversing those two claims gives the same totals when neither
is capped. Under exhaustion, earlier settled claims consume capacity available
to later claims; no unselected reservation or priority guarantee is implied.
This preserves hLP loss eligibility, not an unconditional promise of full payment,
and does not apply borrower principal-only rules to the existing hLP funding loss.
The funding source and LP recipients are now selected: the designated 0.8%
allocation funds insurance under the selected taper, with the remainder using
existing market LP yield distribution, including hLP's underlying yLP ownership.
The hLP target allowance is now selected at one-fifteenth of indexed funding
debt. Target observation and token-fee treatment need explicit implementation
and accounting rules. Do not auto-distribute existing insurance
when its target falls; the accepted curve changes future revenue allocation.

### Recovery at emergency access, 2026-10-09

The user confirmed retaining the agreed margin curve and requested calibration
of emergency access against remaining execution recovery before implementation.
The [new checkpoint report](calibration/EMERGENCY_HEADROOM_RESULTS.md) adds
24,870 observations to the same 2,304 native leverage scenarios. All original
aggregate outcomes are unchanged. It measures full-sale recovery before
insurance/write-off at first ordinary eligibility, first emergency permission
and selected emergency execution.

**Historical recommendation: 75% of effective MM. The user subsequently selected
70% of MM with partial-first emergency recovery and a 120-second incentive ramp.**
The earlier 75% recommendation superseded the two-thirds compromise in light of the priority
of preserving recovery. At 7% MM it permits emergency sale at 5.25% EMA equity;
at 13.67% MM it permits sale near 10.25%. It allows full forced closure sooner
and can worsen paths where later partial fills would have recovered more.
The 10%/7% baseline, stored progressive terms and crowding remain unchanged.

The requested 4–5% decline buffer is achievable in the simplified calculation
with a fixed 2–3% all-in execution haircut from ordinary eligibility. It is not
a guarantee: many native stress checkpoints already have a full-AMM-sale deficit
at ordinary eligibility. Better partial/external execution may still exist.
The experiment used EMA-based permission, partial flash purchases, no timers, and the loss
waterfall. Do not use a poor spot quote alone to authorize an early full sale.
These observations do not resolve stalled ordinary fills above the emergency
boundary, calibrate borrowing positions, or close finding #287652. The caller
reward cap was subsequently selected at 1%; its detailed curve, the insurance
contribution and dust policy still need completion. Runtime is unchanged.

### Emergency threshold comparison, 2026-10-08

The user authorized comparing 75%, two-thirds and 50% of MM. The new
[2,304-run native comparison](calibration/EMERGENCY_THRESHOLD_RESULTS.md) uses
current runtime admission/stored terms, removes the withdrawn solvent shortcut,
models internal principal netting, and covers concentration, hLP, both debt
directions, partial fills, progressive rewards, transfer fees, withdrawals,
declines, gaps, rebounds and alternative keeper ordering. Each full debt leg
clears atomically in the economic adapter; actual flash instructions are still
unimplemented. Accrued interest and external routes are not simulated.

The initial recommendation was **two-thirds of MM as a starting candidate**;
the 2026-10-09 recovery analysis above supersedes that recommendation. Earlier
access generally improves recovery on falling paths but can
worsen outcomes by selling a larger remaining position before later partial
fills become profitable. No tested threshold guarantees cleanup: eligible debt
can remain above the emergency boundary when ordinary Dusk execution cannot
meet the payment floor. Some residual positions are material (750 debt tokens
in this fixture), so this cannot all be dismissed as dust. The health-based
incentive ceases rising if health stabilizes. This is an unresolved execution
risk. The later accepted time-based incentive addresses that limitation but
requires new calibration; the withdrawn early full-sale shortcut stays withdrawn.

The historical recommendation below is superseded where it still specifies
endpoint crowding or a 0–2% solvent-close window.

The [2026-10-07 parameter recommendation](calibration/PARAMETER_RECOMMENDATION.md)
fits the selected 6x reference target with 7/12/17% marginal bands and compares
the remaining settings in 1,080 native scenarios. A separate 48-case diagnostic
identifies keeper cost as the immediate modeled obstacle for all 32 eligible
residual positions from the previous 22 unfinished runs. The recommended
numeric package and dust funding policy are **pending user decisions**; they
do not change the confirmed choices or runtime defaults below.

The eight product questions are answered. The user subsequently selected the 6x
target, 3-percentage-point IM buffer, proposed 0.5%-to-5% discount candidate,
and the remaining-position MM +2-percentage-point partial recovery target.
Other numeric examples are not approvals of production defaults. Calibrate
maintenance rates/boundaries, the liquidity denominator and observation rules,
IM crowding slope, critical-health loss threshold, insurance fee
and dust behavior; validate the selected discount candidate across these choices.

The user's subsequent 6x selection fixes the target near 30% of the stored
side reference. Refit the candidate margin curve to this target and compare its
execution and loss behavior; do not continue treating either previously tested
steep curve as the approved default. Keep the selected 3-percentage-point entry
buffer; the detailed size curve, including beyond that target, remains subject
to calibration. The user considered a 2:1 IM/MM relationship and chose to retain
the additive buffer, with leverage and liquidation price carrying the main UX.

Use the selected policy in new experiments. The 2026-10-02 experiments include
superseded timers, a reference-plus-executable liquidation gate, separate cash
rewards and alternative IM relationships; their results cannot be presented as
validation of this design. Preserve their provenance.

The [stored-depth comparison](calibration/STORED_DEPTH_RESULTS.md) now tests gentle
first bands with steeper large-position tiers in native entry and price-path
quotes. It also checks same-slot LP depth inflation and withdrawal-independent
stored bands. [Fixed-payment economic scenarios](calibration/FLASH_PURCHASE_RESULTS.md)
compare realized model losses, outside depth and position splitting. Neither
experiment selects defaults or establishes implementation safety. In particular,
earlier EMA eligibility does not guarantee a profitable purchase at an EMA-based
payment floor; calibrate pricing and emergency permission together with margins.

The [repayment-cushion comparison](calibration/REPAYMENT_CUSHION_RESULTS.md)
uses the selected 0.5%-to-5% discount candidate and a provisional 7/12/17% margin
curve meeting the 6x target at 30% stored side depth. It compares solvent windows,
separate loss gates, outside depth, withdrawals and competing full/partial order.
It does not implement the runtime, select defaults or establish manipulation safety.

The [native concentrated comparison](calibration/CONCENTRATED_CUSHION_RESULTS.md)
adds actual curve/fee/controller/hLP execution to the repayment-cushion study.
Record rejected trades and remaining debt as well as completed recovery. The
previous hLP reserve-reconciliation failures are fixed by retaining recorded
starting debt and matching the controller's projected reserve state to execution.
The full native replay now has no such guard failures; cash constraints and
integration of the new instruction paths remain to be validated.
The [flash contract](FLASH_LIQUIDATION_CONTRACT.md#native-execution-and-integration-gates)
tracks these gates alongside principal-first interest handling and atomic shared-state settlement.

For partial fills, prove improvement after fees using the surviving position's
maintenance requirement; do not require every small fill to restore the whole
target in one transaction. Test the near-insolvency region where a discount can
make a partial worsen health, and bound full-sale permission and incentives there.
Do not silently choose a subsidy or minimum fee to solve uneconomic dust.

## Implementation order and acceptance evidence

### Current implementation checkpoint, 2026-10-11

Implemented on the #45 branch, with no deployment:

- Stored depth and retained crowding admission terms remain as specified in
  [stored leverage margins](STORED_LEVERAGE_MARGINS.md). The 2% unwind cap is removed.
- Borrowing and leverage share fixed-payment begin/settle instructions, committed
  distress observations, explicit position locks, net token accounting and an
  optional internally netted Dusk AMM route. The old public auction and full
  leverage liquidation entrypoints are removed.
- Emergency AMM settlement is partial-first at 70% of MM. Full closure requires
  nonpositive EMA equity or a conservative proof across integer partial sizes.
  A failed quote, a dust threshold or inability to reach MM+2 does not grant it.
- Principal-first recovery, unpaid-interest cancellation, 75% principal coverage,
  the shared 50% insurance window, P/20+H/15 target, tapered funding and the
  1% solvent fee are wired through actual custody and LP/hLP accounting.
- SDK `dusk.liquidations` exposes previews, observation, atomic route construction,
  internal settlement and emergency builders. Pure helpers expose incentives,
  fees, insurance, gross payment and v0 compilation. Regenerated IDLs and events
  are the integration contract for keepers, the UI and indexer.

#### Emergency reward curve, approved for implementation

The user approved the greater of the following rates, capped at 100 bps (1%) of actual
spendable internal sale proceeds:

```text
health_rate_bps = 100 * clamp(1 - equity_fraction / maintenance_fraction, 0, 1)
time_rate_bps   = 30 + 70 * clamp(distress_age_seconds / 120, 0, 1)
reward_rate_bps = max(health_rate_bps, time_rate_bps)
```

At the selected emergency boundary (equity = 70% of MM), the health component
is 30 bps. If newly observed there, the total starts at 0.30%, reaches 0.65%
after 60 seconds and 1% after 120 seconds if health is unchanged. Equity at
50% of MM gives at least 0.50%; nonpositive EMA equity gives 1% immediately.
Use the same committed ordinary-liquidation distress episode as the buyer
discount, not a new emergency-start clock. Existing episode age can therefore
make the first emergency call eligible for more than 0.30%, up to the 1% cap.
Useful partial fills do not reset continuing distress; verified recovery ends
the episode. Elapsed time never bypasses the selected emergency-health gate
or the full-close permission. Quote the rate from pre-sale health/time and
include the reward in post-cost partial improvement and loss accounting.

This is the selected starting curve, not a statistically optimal incentive.
It reuses the selected two-minute ordinary
distress horizon, provides a nonzero initial emergency reward, and pays the
maximum immediately for reference-insolvent positions. Arithmetic checks
confirm the range and monotonicity; integrated recovery/keeper analysis still
needs to validate it with the complete new policy.

### Sizing, rounding and computation

Ordinary partial requests cap repayment to preserve the configured minimum debt.
A fill crossing MM+2 must be the first permitted integer share burn to reach it;
checking only the previous atom is insufficient because rounding can create
multiple crossings. Larger requests return `LiquidationRepayTooLarge`; callers
must use a smaller quote. Useful below-target fills remain permitted.

Emergency callers select collateral input. Oversized or subminimum-residual
quotes reject and must be resized. The recovery cap uses a conservative
fee-free AMM bound over smaller slices, so it may reject a target-crossing
request even when that exact smaller fill would not cover all actual costs.
A useful below-target fill remains available when executable. The full-close
proof similarly considers optimistic flash and AMM repayments. It ignores dust
exclusions, rejects if any partial might help, and refuses permission when its
bounded search cannot establish impossibility. Neither certificate grants a
loss permission on compute exhaustion. This is intentionally conservative,
not a guarantee that every eligible position can be profitably cleared.

Dust minimums are separate Base/Quote raw-atom market creation parameters.
Their default is one atom; launchers must choose an economically meaningful
amount for each market. No dollar denomination or fixed cleanup subsidy is
assumed. The insurance window retains the existing slot-based approximation
of 24 hours. No independent hLP spending allowance is added.

### Verification and integration boundary

The complete required local CI sequence passed on this implementation:

- Formatting, staged-file hygiene, code shape and required Clippy checks.
- 489 Dusk tests in each default/production profile; production library check; 16 delegate tests and 1 faucet test.
- Full Anchor/SBF builds (Dusk, delegate, faucet and fixtures), clean build identity, all four regenerated SDK interfaces and TypeScript checking.
- 56 SDK tests, including debt-token funding/payout builders and rejection of removed funding-mode flags.
- Strict LiteSVM: 131 passing, no pending tests, all 67 instructions exercised and the complete compute baseline required.

The benchmark feature also compiles. These are implementation/accounting regressions, not an independent security audit or evidence of economic optimality. No historical exploit PoC was executed.

Native tests cover both debt directions, share-floor changes during a session,
partial recovery, oversize rounding, minimum residuals, positive-equity full
proofs, principal/interest/insurance accounting and concentrated hLP loss backing.
Instruction tests cover external and internal flash execution, exact pairing,
underpayment rollback, locked-position mutation, independent repayment and LP
withdrawal during a route, committed-clock recovery resets, actual Token-2022
collateral/debt fees, prior payment donations, hLP fee participation and emergency
partial/full settlement. SDK checks include exact indices and custom program IDs.

The historical 576/2,304 scenario studies and #51 replay do not validate this
combined policy. A fresh economic calibration, an independent redesign audit,
downstream keeper/UI/indexer adoption and devnet rollout remain release work.
The superseded #287652 auction backstop entrypoint is removed; the new explicit
emergency permission still accepts execution-price risk at critical EMA health.
It is not a claim that AMM price manipulation or keeper liveness is solved.

No merge or deployment is part of this implementation request.
