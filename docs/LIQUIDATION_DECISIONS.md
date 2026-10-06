# Margin and flash liquidation decisions

Updated 2026-10-07. This is the current decision record for implementation in
[PR #45](https://github.com/omnipair/dusk/pull/45). It supersedes conflicting
pricing, eligibility, reward, clock and composability proposals in the earlier
handoff, flash contract and calibration documents. Decisions are not claims of
implemented or tested behavior.

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
| Partial liquidation | Accept useful partial fills toward a recovery target. Size the maximum seizure to avoid unnecessary liquidation after recovery; full liquidation for insolvency, infeasible recovery or uneconomic leftovers under explicit rules. |
| Buyer economics | Quote a specified collateral slice for a specified net debt-token payment. The buyer can route anywhere, pay from inventory, or retain purchased collateral; upside on the purchased slice belongs to the buyer. Unsold collateral remains position-owned. |
| Incentive | User accepted the proposed starting discount curve: 0.5% at the liquidation threshold, rising linearly with deteriorating reference health to 5% at zero EMA equity, capped at 5% when insolvent. Validate this candidate jointly with partial recovery and emergency execution; it does not guarantee profitable fills. Competitive keeper racing is an accepted assumption. The earlier separate bounded cash reward and mandatory return of all execution upside are superseded. |
| Timing | Immediate eligible fills; no required auction wait, expiry, distress timer or time-based emergency unlock. |
| Earlier solvent AMM close | User approved modeling a full internal sale while its net repayment cushion is small but nonnegative, after bounded caller reward and applicable costs. Ordinary EMA eligibility remains necessary; actual execution must repay principal plus all accrued interest without insurance or write-off. Return residual to owner. Cushion width is unselected; 2% is a comparison candidate. |
| Loss-taking emergency | Keep a separate permission for full Dusk-AMM sale below the ordinary flash-payment floor at critically low EMA equity. Half maintenance was illustrative, not a selected threshold. A negative AMM repayment cushion alone does not authorize insurance/write-off. An arbitrary external route cannot lower its bound payment after release. |
| Emergency caller | Pay a bounded progressive share of actual internal sale proceeds, including insolvent sales. This compensates the caller when Dusk performs the sale itself; the percentage is unselected and 1% was illustrative. |
| Loss waterfall | Full-liquidation recovery protects principal first. Insurance covers remaining principal only within applicable limits. Remaining principal is written off, unpaid interest is canceled, and canceled interest generates no protocol/referral fees. Finish with zero debt on the fully settled leg. |
| Insurance funding | Successful solvent borrowing and leverage flash liquidations contribute a bounded insurance fee. Include it in the quoted borrower cost and recovery math; it must not create a shortfall merely to fund insurance. |
| Composability | Lock the liquidated position and its bound obligation. Allow other-position swaps, borrowing, leverage and independent liquidations. Also allow LP withdrawals and relevant parameter changes with additional accounting checks; do not introduce a blanket same-market ban. |
| Ordinary leverage execution | Preserve ordinary Dusk AMM execution. General external owner open/close routing is outside the agreed implementation scope. |
| PR coordination | Incorporate all of #48's native-collateral feature and fixes into #45. Preserve the audit remedies already in #45. |

## Settlement invariant

```text
begin: confirm eligibility; bind collateral X and net debt payment Y
       keep original debt/exposure/reservations live; activate position session
       release X collateral
buyer: route freely or supply its own funds, retaining its purchased-slice upside
settle: verify the original session and actual net reserve credit >= Y
        apply the selected insurance fee and debt reduction
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

The additional solvent AMM mode measures executable repayment after the bounded
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

## Calibration and implementation details still to resolve

The [2026-10-07 parameter recommendation](calibration/PARAMETER_RECOMMENDATION.md)
fits the selected 6x reference target with 7/12/17% marginal bands and compares
the remaining settings in 1,080 native scenarios. A separate 48-case diagnostic
identifies keeper cost as the immediate modeled obstacle for all 32 eligible
residual positions from the previous 22 unfinished runs. The recommended
numeric package and dust funding policy are **pending user decisions**; they
do not change the confirmed choices or runtime defaults below.

The eight product questions are answered. The user subsequently selected the 6x
target, 3-percentage-point IM buffer and proposed 0.5%-to-5% discount candidate.
Other numeric examples are not approvals of production defaults. Calibrate
maintenance rates/boundaries, the liquidity denominator and observation rules,
IM crowding slope, recovery target, solvent repayment-cushion width,
critical-health loss threshold, insurance fee
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

### Margin implementation checkpoint, 2026-10-07

The current increment implements stored position terms, aggregate collateral
exposure, incremental admission obligations, symmetric-EMA eligibility, and SDK
display helpers/events. It replaces the fixed 2% unwind rejection. Development
defaults use 7/12/17% marginal MM at 5/15% of stored depth, plus the selected
3-point buffer and a growing crowding tail. The exact potential, rounding and
lifecycle rules are in [the implementation contract](STORED_LEVERAGE_MARGINS.md).
This supersedes the earlier endpoint-crowding arithmetic for runtime admission.

Native tests cover actual openings across concentration, controller/hLP and
both debt directions, retained-equity withdrawal checks, LP activity, partial
closure and full repayment. These lifecycle tests do not replace loss-path
calibration. Atomic flash sessions, partial liquidation, emergency permission,
progressive rewards and the selected loss waterfall remain unfinished. Existing
full-sale liquidation and auction instructions still exist; finding #287652
remains open. No deployment is included.

### Remaining integration sequence

1. Combine #48 into #45 and reconcile audit, native-collateral, SDK and test
   changes. Regenerate interfaces; run all required CI checks before committing.
2. Calibrate the selected economics and publish proposed numeric defaults with
   their loss and leverage tradeoffs. Preserve the V2 EMA/liquidity protections.
3. Implement explicit sessions, shared accounting and fixed-payment quotes for
   borrowing and leverage; replace auctions and add partial settlement.
4. Integrate progressive margins, emergency liquidation, insurance funding and
   principal-first loss accounting into instructions and previews.
5. Exercise instruction-level begin/settle rollback, independent sessions,
   LP withdrawals, parameter changes, same-market reborrowing, account lifecycle,
   transfer fees, insolvency, native collateral and owner/delegate payouts.

Verified before consolidation: #45 head
`5b123acf30574473d4a9346fbd1195d6d6cf1733`; #48 head
`4f4f74cbb05edc40e9ed074672d2d8a941f1b5d6`; origin/main
`e8f12ea78922d305a2888b895b7d8fbe0994bc01`. Recheck remote heads before pushing.
