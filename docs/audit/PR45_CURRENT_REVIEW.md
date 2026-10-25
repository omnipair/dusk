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

### Public principal follows each borrower

The review identified that `BorrowPosition` stored shares but no principal.
Repayment and flash liquidation classified one position using the **aggregate**
principal/debt ratio, allowing unrelated loans to change its insured principal.

Ignoring atom rounding: Alice borrows 1,000 and accrues debt to 1,250. Bob
borrows and immediately repays 1,000. The side now has 2,250 debt and 2,000
principal before Bob's repayment. The pooled split classifies Bob's 1,000 as
888 principal and 112 interest. Alice's remaining 1,250 debt is consequently
booked as 1,112 principal and 138 unpaid interest, though Alice originally
received 1,000 principal. If Alice's collateral later realizes 900, a 75%
principal-loss policy can draw 159 of insurance rather than 75, subject to
the fund's draw limit. This is classification drift, not newly created debt.

The remedy records `fixed_base_principal` and `fixed_quote_principal` on each
borrow position. Borrow increases both the position and market totals; repay,
legacy liquidation, flash liquidation and emergency settlement reduce both by
the same position-derived amount. Debt shares, the borrow index, utilization
and reserves remain pooled. The SDK exposes `borrowDebtBreakdown` for displaying
one position's principal and accrued interest. A regression opens Alice's old
loan, accrues interest, opens and fully repays Bob's fresh loan, then checks
Alice's principal and principal-only insurance quote in both asset directions.

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
each pass 537 tests on the integrated branch. The hLP flash tests cover both
sides, 1x/4x/10x
concentration, funded solvent recovery, terminal principal-only insurance,
frozen-vault rescue and coexisting flash sessions. These tests and this scoped
review do not prove universal route availability, economic profitability or
all possible transaction compositions. Delegate tests pass 16, faucet passes
one, SDK previews pass 77, and the current LiteSVM run passes 141 tests.
Formatting, hygiene, code shape, Clippy, production compilation and TypeScript
also pass. The final LiteSVM build, regenerated SDK interface check, SDK
preview suite and strict complete-CU LiteSVM run also pass on this source.
The public-principal classification issue above is included in this remedy.

### Native terminal hLP recovery reference

A later local review found that `close_insolvent_hlp` used the live concentrated
reserve ratio to value the target-token claim. A same-slot price-moving swap
could therefore change principal recovery, insurance and writeoff even though
terminal eligibility separately required symmetric-EMA insolvency.

The native terminal waterfall now refreshes linear symmetric-EMA eligibility
to the instruction's current slot and
quotes the target claim on the same synthetic emergency curve used by borrowing
and leverage: symmetric EMA price, `curve_depth_ema_nad`, configured
concentration and full swap fees. The borrowed-token claim is added directly.
The quote-only path does not construct an active hLP transition, so the
insolvent vault cannot block pricing its own close. External flash terminal
settlement uses the caller's actual fixed payment and the borrowed-token yLP
claim bound at begin, so an interleaved Dusk swap cannot change the session's
loss allocation. The complete begin/route/settle transaction must still fit
Solana's compute limit. Native Dusk recovery remains available through the
ordinary swap path, and flash callers must simulate direct Dusk and external
routes. Real reserves are
rebuilt after the native close, preserving the already accepted inventory and
later-arbitrage tradeoff.
