# PR #45 integrated closeout review

Initial reviewed source: `05585b9c` on 2026-10-24. This is a scoped review of the
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

## Verified behavior and disposition

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

### Retired hLP receipts and new deposits

Both terminal close paths zero the vault's yLP and debt while leaving the
existing hLP receipt supply in place so holders can claim already-funded yield
and burn the receipts. A subsequent deposit sees nonzero supply but zero old
NAV. `deposit_single_sided` calls `hlp_shares_for_delta_nav`, which requires a
positive old NAV, so entry fails. This keeps old receipts from acquiring new
principal, but a lost or uncooperative final holder can delay future hLP entry
on that side indefinitely. A full direct token burn of the old receipts is
also rejected by `reconcile_live_hlp_supply` while stored supply is nonzero.

The user selected a new receipt cohort. The remedy adds
`rotate_hlp_cohort` after a terminal close: it freezes the old hLP vault's
yield indexes in a mint-specific cohort account, changes the market to a fresh
zero-supply hLP mint and starts a fresh vault. Old receipts can still transfer,
harvest their previously earned yield and burn; they cannot acquire principal
from new deposits. The instruction regression executes terminal close, receipt
rotation, transfer of an old receipt, a new deposit, burns by both old holders
and an old holder's funded-yield harvest. All 141 LiteSVM instruction tests
pass with 76/76 instructions exercised and the strict compute baseline.

## Checks and limits

The original OR-NM-004 negative curve-only equity fixture now passes with
positive full-value hLP backing. The full default and production Dusk suites
each pass 536 tests on the integrated branch. The hLP flash tests cover both
sides, 1x/4x/10x
concentration, funded solvent recovery, terminal principal-only insurance,
frozen-vault rescue and coexisting flash sessions. These tests and this scoped
review do not prove universal route availability, economic profitability or
all possible transaction compositions. Delegate tests pass 16, faucet passes
one, SDK previews pass 76, and the current LiteSVM run passes 141 tests.
Formatting, hygiene, code shape, Clippy, production compilation and TypeScript
also pass. The final LiteSVM build, regenerated SDK interface check, SDK
preview suite and strict complete-CU LiteSVM run also pass on this source.
The public-principal classification issue above remains a separate decision.
