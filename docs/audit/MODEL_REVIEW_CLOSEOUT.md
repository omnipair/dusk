# External model review: remediation status

Updated 2026-10-19. Original reviewed head: `3d9bae7f`. Follow-up base:
`5c0aca7f`. **PR #45 is not ready to merge or deploy.**

The original review locally verified seven findings. The external model runs
did not all complete the full Nemesis workflow. This document records source
fixes and remaining work; it is not a new exhaustive audit or a security signoff.

## Confirmed findings

| Finding | Disposition |
| --- | --- |
| OR-NM-001: fixed Token program SDK slots | Fixed in `f2794e76`; actual account construction is covered by `check-write-program-accounts.js`. |
| OR-NM-002: governance execution mint accounts | Fixed in `f2794e76`; the same SDK check builds the instruction with both required mints. |
| OR-NM-003: partial-close principal rounding | Fixed in `d97e6997`; remaining principal follows aggregate before/after debt clearance. |
| OR-NM-004: hLP solvency/recovery disagreement | **Partially remediated in #52.** Full-value entry/NAV/hedge reconstruction corrects the original valuation disagreement. Cash-funded harvest sequencing and vault-wide external recovery are implemented in the follow-up. Final economic and adversarial review remains a closeout gate. |
| OR-NM-005: gross close/decrease cash preflight | Fixed in `f2794e76`; preflight includes exact principal netting and retains actual cash floors. |
| OR-NM-006: stale protection EMA | Fixed in `f2794e76`; the disposable protection snapshot refreshes reference risk. |
| OR-NM-007: stale hLP settlement reference | Exit guard fixed in `f2794e76`; fresh deposit NAV in `5c0aca7f`. The separate full-value entry correction is implemented in #52; the remaining harvest/recovery work is tracked under OR-NM-004. |

## Follow-up findings

### Zero-value hLP receipt cleanup

After a terminal waterfall, the vault has no yLP, principal or funding debt.
Its receipt tokens remain burnable, and already funded yield is checkpointed
to their holders. The withdrawal handler nevertheless finalized the AMM and
checked global health after burning, so a different distressed hLP vault could
revert this zero-payout cleanup.

The fix skips market accrual, AMM finalization and global admission checks only
when the vault is already terminally closed. It verifies that the receipt has
zero yLP burn, payout, debt repayment and interest payment. Token ownership,
mint/custody validation, funded-yield accounting, token burn, custody checks and
the owner's minimum payout still apply. Ordinary withdrawals keep their checks.

The instruction regression first closes Base hLP, then injects a funding-index
shock into the independent Quote hLP. The old binary rejects cleanup with
`HlpSettlementUnavailable`. The corrected binary burns the retired receipt with
unchanged reserves, owner cash, debt and the other vault.

### Flash-plan simulation indices

`simulateWithContext` prepended heap/compute instructions even when a flash plan
already contained them. This both duplicated compute-budget settings and moved
the absolute settlement index embedded by `buildFlash`.

Simulation now keeps complete flash plans at their original indices; explicit
overrides replace existing budgets in place. Incomplete index-bound plans and
duplicate supplied budgets are rejected. Ordinary previews retain their budget
prefix so Solana return data still comes from the preview. Tests compile a real SDK flash plan
with an address lookup table and inspect the simulated message's begin/settle
instructions and budget counts. An actual SVM preview verifies return data.
Simulation never signs or submits a transaction.

## Validation of this follow-up

The complete local CI sequence passed on the candidate changes, including the
production Rust profile, Dusk/delegate/faucet builds, build identity, SDK checks
(69 passing), and LiteSVM with complete CU coverage and no pending tests
(135 passing; 71/71 instructions exercised). The native SDK preview caught an
intermediate regression where trailing compute-budget instructions replaced
the return-data program; the final implementation retains the ordinary preview
prefix and passes that regression.

These checks validate the narrow changes above. They do not close OR-NM-004,
the hLP entry mismatch, or the remaining unverified model hypotheses. A fresh
full-PR Nemesis review and rollout checks remain pending.

## Remaining hLP accounting work

The selected policy is unchanged: full accrued public-interest value for LP
ownership, cash-limited exits without a separate unpaid claim after exit, and
public unpaid interest excluded from AMM pricing reserves.

The accounting correction gives hLP entry and reconstruction the same
full-value backing as yLP. It keeps separate book offsets outside swap-pricing
coordinates, retains reserve/cash checks, and marks opposite-asset residuals
at the concentrated curve price. Native regressions cover both hLPs, both
interest assets, concentration, repayment and writeoffs, and positive-value
recovery from the old curve-only insolvency condition.

A further local economic regression found a repeatable fee-free round-trip
credit when the book-value candidate reconstructed ownership after pricing.
The revised candidate certifies the actual reconstructed endpoint before
choosing trader output. It passes repeated-cycle checks at two concentration
levels and both trade directions. The complete local CI sequence now passes:
513 default/production Dusk tests, 70 SDK checks and 136 strict LiteSVM tests,
including real public-debt/funding accrual and both-asset round trips. All 18
required compute scenarios are covered. The full review remains a gate.
The accounting note records the material concentrated hLP compute increase.

This is partial remediation, not closeout. The follow-up native advance ledger
freezes the LP/protocol split when reserve cash funds interest, reconciles paid
and canceled advances, and includes SDK accounting helpers. The follow-up now exposes cash funding and SDK harvest builders, and settles
new hLP income before publishing holder yield. Real instruction tests cover
yLP and both hLPs, concentration, transfer fees and repayment. External hLP flash execution and SDK transaction builders are now implemented,
with atomic payment, collateral-fee and rollback coverage. Native and flash
terminal settlement use principal-only insurance and separately cancel unpaid
interest. Full review of this expanded recovery surface remains required.
The completed harvesting/recovery implementation passes 523 Dusk tests in each
default/production profile, 75 SDK checks, and 137 strict LiteSVM tests covering
all 75 instructions. Real funding, harvest and repayment run with both hLPs,
transfer fees and a changed protocol rate; flash tests cover inventory and
Dusk-AMM routes, insolvent closure, underpayment and atomic rollback. Existing compute
ceilings remain unchanged. No merge or deployment
should rely on native tests alone. The recovery target is confirmed as
opposite-token neutrality, including when concentration makes gross leverage
differ from 2x.

Other model hypotheses remain unverified, including terminal cohort restart,
orphaned final-token burns, borrower principal pooling and delegated execution
price constraints. They must not be described as disproved or remediated based
only on absence from the original seven verified findings.

The original V12 dispositions, including accepted residual inventory/arbitrage
risk for #287652 and issuer/keeper assumptions for #287160, remain recorded in
[V12_FINDINGS.md](V12_FINDINGS.md).
