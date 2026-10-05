# Margin and flash liquidation decisions

Updated 2026-10-05. This is the current decision record for implementation in
[PR #45](https://github.com/omnipair/dusk/pull/45). It supersedes conflicting
pricing, eligibility, reward, clock and composability proposals in the earlier
handoff, flash contract and calibration documents. Decisions are not claims of
implemented or tested behavior.

## Confirmed choices

| Topic | Selected behavior |
| --- | --- |
| Ordinary liquidation eligibility | Symmetric EMA health alone. Preserve V2's linear collateral valuation: hypothetical AMM price impact and LP withdrawal do not independently make a reference-healthy position liquidatable. Preserve the separate scheduled-transfer-fee remedy. |
| Admission valuation | Preserve directional/asymmetric EMA protections for new risk and existing cash/accounting constraints. Deliberate use of different EMA roles is not a finding by itself. |
| Initial margin | At least maintenance plus a calibrated buffer; aggregate exposure on the same side may raise the requirement further. Apply to new or increased risk. No replacement hard cap at 50% TVL. |
| Maintenance | Progressive position-size bands scaled to a conservative liquidity reference stored at entry. LP withdrawals and other traders' entries do not rebase an existing position's bands. Risk increases recheck applicable terms and expose changes in the preview. |
| Calibration priority | Preserve gentle small-position requirements near today's 10% IM / 7% MM; test substantially steeper marginal maintenance for large positions relative to stored liquidity. Exact boundaries and rates remain unselected. |
| Partial liquidation | Accept useful partial fills toward a recovery target. Size the maximum seizure to avoid unnecessary liquidation after recovery; full liquidation for insolvency, infeasible recovery or uneconomic leftovers under explicit rules. |
| Buyer economics | Quote a specified collateral slice for a specified net debt-token payment. The buyer can route anywhere, pay from inventory, or retain purchased collateral; upside on the purchased slice belongs to the buyer. Unsold collateral remains position-owned. |
| Incentive | Progressive collateral discount based on deteriorating reference health, including insolvent cleanup. Competitive keeper racing is an accepted assumption. The earlier separate bounded cash reward and mandatory return of all execution upside are superseded. |
| Timing | Immediate eligible fills; no required auction wait, expiry, distress timer or time-based emergency unlock. |
| Emergency | Permit a full Dusk-AMM sale below the ordinary flash-payment floor at critically low EMA equity. Half maintenance was illustrative, not a selected threshold. An arbitrary external route cannot lower its bound payment after release. |
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

## Calibration and implementation details still to resolve

The eight product questions are answered. Numeric examples were not approvals of
production defaults. Calibrate maintenance rates/boundaries, the liquidity
denominator and observation rules, IM crowding slope and buffer, recovery target,
discount curve, critical-health threshold, insurance fee and dust behavior.

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

For partial fills, prove improvement after fees using the surviving position's
maintenance requirement; do not require every small fill to restore the whole
target in one transaction. Test the near-insolvency region where a discount can
make a partial worsen health, and bound full-sale permission and incentives there.
Do not silently choose a subsidy or minimum fee to solve uneconomic dust.

## Implementation order and acceptance evidence

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
