# PR #45 integrated closeout review

Reviewed source: `05585b9c` on 2026-10-24. This is a scoped review of the
integrated hLP accounting/recovery and its shared debt accounting, not a new
claim that every changed line in the PR has independent security signoff.

## Scope and state map

The review followed hLP deposit, withdrawal, direct-burn reconciliation,
interest funding, spot swap reconstruction, frozen-vault recovery, native
terminal closure, external flash begin/settle and the shared public-debt
repayment/liquidation ledger. The coupled state checked was:

| Coupled state | Required relationship | Mutating paths checked |
| --- | --- | --- |
| hLP receipt supply and vault-owned yLP/debt | A live receipt cohort owns the matching vault claim and liability | Deposit, withdraw, direct burn reconciliation, terminal close |
| Live reserve, executable cash, unpaid interest and flash receivable | Book value and custody stay backed while unpaid interest and flashed collateral remain non-executable | Funding, swap, flash begin/settle, terminal close |
| yLP and hLP yield indexes and supply | Historical yield is checkpointed before ownership or denominator changes | Deposit, withdraw, transfer hook, direct burn reconciliation, funding, terminal close |
| Fixed debt shares and principal | Repayment and liquidation classify collected cash and insured loss consistently with the position retired | Borrow, repay, flash liquidation, emergency settlement |
| Frozen hLP mask and debt/ownership reconstruction | A distressed vault is not silently rehedged or erased by an ordinary swap | Swap, flash recovery, terminal close |

The Feynman pass questioned guard parity, terminal restarts, per-position versus
pooled principal, cash floors and begin/settle ordering. The state pass traced
the same coupled values through their parallel normal and terminal paths. A
targeted second pass checked whether lazy yield checkpointing or zero-value
receipt cleanup resolved the suspected gaps.

## Verified behavior requiring a decision

### Public principal is pooled across borrowers

`BorrowPosition` stores shares but no principal. Public borrowing increments
`Debt.fixed_*_principal`; repayment calls `realize_margin_liquidation`, which
splits cash at the **aggregate** principal/debt ratio. Flash liquidation also
derives the position's principal from that aggregate ratio. Thus the amount
insured as principal for one borrower depends on unrelated borrowers' actions.

Ignoring atom rounding: Alice borrows 1,000 and accrues debt to 1,250. Bob
borrows and immediately repays 1,000. The side now has 2,250 debt and 2,000
principal before Bob's repayment. The pooled split classifies Bob's 1,000 as
888 principal and 112 interest. Alice's remaining 1,250 debt is consequently
booked as 1,112 principal and 138 unpaid interest, though Alice originally
received 1,000 principal. If Alice's collateral later realizes 900, a 75%
principal-loss policy can draw 159 of insurance rather than 75, subject to
the fund's draw limit. This is classification drift, not newly created debt.

The code path is `lending/mod.rs::borrow` → `Debt::add_margin_principal` →
`lending/mod.rs::repay_with_finalization_and_fee` →
`Debt::realize_margin_liquidation` →
`flash_settlement.rs::liquidation_debt_allocation` →
`LiquidationLossAllocation::quote_with_coverage`. Whether principal protection
should follow each borrow position is a product/accounting decision; no source
change is claimed here.

### Retired hLP receipts prevent a new deposit cohort

Both terminal close paths zero the vault's yLP and debt while leaving the
existing hLP receipt supply in place so holders can claim already-funded yield
and burn the receipts. A subsequent deposit sees nonzero supply but zero old
NAV. `deposit_single_sided` calls `hlp_shares_for_delta_nav`, which requires a
positive old NAV, so entry fails. This keeps old receipts from acquiring new
principal, but a lost or uncooperative final holder can delay future hLP entry
on that side indefinitely. A full direct token burn of the old receipts is
also rejected by `reconcile_live_hlp_supply` while stored supply is nonzero.

The source trace is `flash_terminal.rs::settle_terminal_hlp_flash` or
`engine.rs::HlpTerminalWaterfallPlan::consume` →
`engine.rs::deposit_single_sided` → `hlp_shares_for_delta_nav`, with
`instructions/liquidity/hlp/mod.rs::reconcile_live_hlp_supply` on the token
path. Fresh cohort identity versus waiting for every old receipt to be burned
is pending a product decision; no source change is claimed here.

## Checks and limits

The original OR-NM-004 negative curve-only equity fixture now passes with
positive full-value hLP backing. The full default and production Dusk suites
each pass 543 tests. The hLP flash tests cover both sides, 1x/4x/10x
concentration, funded solvent recovery, terminal principal-only insurance,
frozen-vault rescue and coexisting flash sessions. These tests and this scoped
review do not prove universal route availability, economic profitability or
all possible transaction compositions. On this reviewed source, both Anchor
program builds completed, regenerated SDK interface files matched the tracked
files, all 76 SDK preview checks passed, and LiteSVM passed 140 instruction
tests with 74/74 instructions exercised and the complete compute baseline.
Formatting, hygiene, code shape, Clippy, production compilation, delegate and
faucet tests, and TypeScript typechecking also passed. A later source change
requires these gates to run again before its commit.
