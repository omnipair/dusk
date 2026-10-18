# External model review: remediation status

Updated 2026-10-18. Original reviewed head: `3d9bae7f`. Follow-up base:
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
| OR-NM-004: hLP solvency/recovery disagreement | **Open.** Ordinary curve equity excludes public unpaid interest; terminal backing includes it. A vault can fail ordinary operations while terminal recovery rejects it as solvent. |
| OR-NM-005: gross close/decrease cash preflight | Fixed in `f2794e76`; preflight includes exact principal netting and retains actual cash floors. |
| OR-NM-006: stale protection EMA | Fixed in `f2794e76`; the disposable protection snapshot refreshes reference risk. |
| OR-NM-007: stale hLP settlement reference | Exit guard fixed in `f2794e76`; fresh deposit NAV in `5c0aca7f`. The separate unpaid-interest entry mismatch below remains open. |

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

Those rules are not consistently implemented in hLP entry and reconstruction.
New hLP funding uses curve reserves, while its underlying yLP purchase and share
NAV use live reserves. A public receivable can therefore make a deposit create
a hedge residual. Merely changing the funding ratio to live reserves fixed four
of eight local matrix cases; opposite-asset-interest cases still failed. That
incomplete prototype was archived and is not part of the candidate runtime.

Closing OR-NM-004 and this entry mismatch requires testing the complete hedge
transition against ordinary yLP and both hLP vaults' economic value. Neither
relaxing the hedge guard nor excluding earned interest from share NAV is an
approved remedy. No merge or deployment should rely on green CI alone while
these items remain open.

Other model hypotheses remain unverified, including terminal cohort restart,
orphaned final-token burns, borrower principal pooling and delegated execution
price constraints. They must not be described as disproved or remediated based
only on absence from the original seven verified findings.

The original V12 dispositions, including accepted residual inventory/arbitrage
risk for #287652 and issuer/keeper assumptions for #287160, remain recorded in
[V12_FINDINGS.md](V12_FINDINGS.md).
