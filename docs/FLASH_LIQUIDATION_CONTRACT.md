# Flash liquidation settlement contract

**2026-10-05 update:** [current decisions](LIQUIDATION_DECISIONS.md) supersede
conflicting economics below. Use fixed net payment for a collateral slice, with
buyer upside; symmetric-EMA-only ordinary eligibility; useful incremental partial
fills; no mandatory timer; principal-first losses; and a bounded solvent insurance
contribution for borrowing and leverage. LP withdrawals and parameter changes
remain composable with accounting checks. The explicit session, immutable
obligation, transfer accounting and mutation inventory remain implementation
requirements, not completed protections.

Status: implementation specification for PR #45, updated 2026-10-05. The existing program does not yet implement this session or its guards. This document complements the [margin and liquidation plan](LEVERAGE_MARGIN_AND_LIQUIDATION_PLAN.md) and [current decisions](LIQUIDATION_DECISIONS.md).

Emergency permission uses critically low symmetric EMA equity, with its numeric threshold still to be calibrated. There is no mandatory waiting period or distress clock. The [older emergency policy](EMERGENCY_LIQUIDATION_POLICY.md) is historical calibration context. Internal price relaxation never permits an arbitrary external route to return less than its bound obligation.

## Transaction and custody

```text
begin: validate terms and exact later settlement; activate session
    -> release only the authorized collateral
    -> keeper executes a route / explicitly supplies repayment
settle: validate original obligation; measure credit; commit accounting
    -> partial: reduce debt and retain collateral/equity
    -> final: apply sale proceeds, draw eligible insurance, write off any remainder
              clear the liquidated debt leg; return any owner residual
```

Begin, route execution, settlement, any insurance draw, and any final write-off must run in the same atomic transaction. Any failure reverts the entire sequence. The initial implementation should require both endpoints at the top level, bind their exact instruction indices through the canonical instructions sysvar, and reject CPI invocation of either endpoint. This leaves ordinary Dusk instructions callable through their existing interfaces; it does not grant those calls permission to mutate an active position. Verify the chosen runtime introspection API in executable tests before claiming the guarantee.

Use a separate program-controlled session repayment account. Measure its **increase in spendable token amount**, with mint, token program, authority, delegate, close authority, and account address validated. Preexisting donated tokens must not count toward the new obligation. Keep this account distinct from the main reserve, input custody, owner payout, keeper payout, and fee/insurance accounts. Reject aliasing that would let one transfer satisfy two obligations or hide a debit. Route proceeds can enter this account; keepers cannot withdraw from it.

Route output credit is not final reserve repayment credit for a transfer-fee debt token. The later repayment-account-to-reserve transfer can charge a second fee. Settle must observe that reserve credit and distinguish shares cleared through repayment from shares cleared through an authorized final write-off. Count each physical transfer once. The buyer owes the bound net payment; it can retain external route upside on the purchased collateral slice. Dusk must separately account for the bounded solvent insurance contribution and debt repayment. Define rounding and accidental overpayment refunds explicitly. Unsold collateral remains position-owned. An owner wallet cannot be debited as an implicit shortfall source.

The user explicitly confirmed full loss resolution: principal-first sale recovery, insurance for remaining principal within its limits, then principal write-off and cancellation of unpaid interest without generating fees on canceled interest. A completed full liquidation leaves zero shares/principal for the liquidated debt leg. A borrowing account with unrelated collateral or another debt leg remains open until its own closure conditions are met. A write-off is not cash yield. The phrase "authorized shortfall" must not mean that a finished full liquidation can strand outstanding debt.

Ordinary flash purchases use a progressive collateral discount, including insolvent cleanup. For an internal emergency sale, pay the caller a bounded progressive share of actual proceeds before recovery. Competitive permissionless keepers racing for profitable fills are an accepted design assumption. Use this assumption in economic calibration; numeric incentive curves and caps remain unselected.

Any buyer-controlled released collateral can be exchanged against inventory or routed through accounts outside Dusk's observation. Dusk enforces the bound collateral release and net payment. The buyer may keep purchased collateral and repay with its own funds. Do not describe this as an on-chain guarantee of best execution or bounded total buyer profit.

## Session identity and obligations

Use a nonzero active-session key on each position, with a session PDA derived from the position and a discriminator/nonce that cannot match an unrelated in-transaction session. Do not encode the lock as zero collateral, zero debt, an auction sentinel, or a spare bit in an unrelated field. The session should bind at least these groups:

| Group | Bound data |
| --- | --- |
| Identity | Position kind/key, market, owner, namespace/position ID or incarnation, session PDA, rent payer, beginning and settlement instruction indices. |
| Token accounts | Collateral/debt mints and token programs; original custody, temporary input if used, repayment vault, reserve destination, owner residual and keeper reward accounts. |
| Original accounting | Original collateral quantity, original debt shares and principal, applicable borrow index/checkpoint, market exposure contribution, and borrowing health contribution where applicable. |
| Economic terms | Exact gross collateral authorized, observed release credit, reference context, discount, net reserve repayment obligation, bounded insurance contribution, recovery target, permitted partial/final mode, current/scheduled token-fee treatment. |
| Settlement | Spendable repayment balance before routing, approved handling of unspent collateral, authoritative net reserve credit, and emitted partial/final result. |

Write the active lock and session before any collateral transfer or untrusted invocation, including serializing account state before a CPI that can observe it. End must revalidate the same identity and original debt obligation. It cannot use zero debt or a healthy current account as a substitute. Clear the lock only as part of successful authorized finalization; transaction rollback handles any later failure.

Keep debt shares, gross exposure, and committed collateral allocation in force while the session is active. Releasing physical custody must not temporarily improve borrow capacity or make debt eligible for write-off. At settlement, atomically update actual debt, custody allocation, global borrowing contributions, leverage exposure, insurance, fees/referrals, and the curve/hLP accounting affected by real reserve credit. A receivable written off is not repaid interest or cash yield.

Account for shared-market changes between the endpoints. A position lock is insufficient if another instruction can spend an in-flight asset or count it as free liquidity. Conversely, a blanket market lock would prevent a route from using Dusk's AMM. Prefer keeping existing obligations/reservations live and proving that ordinary spot/other-position operations preserve them. If that cannot be demonstrated for a path, identify the exact blocked operation and its product consequence before introducing a broader restriction.

## Mutation inventory

The baseline inventory below is from `programs/dusk/src/lib.rs` at PR #45 head `5b123acf`. Existing identity checks and `require_open()` are not active-session checks today. Guards must protect both public handlers and reusable state transitions that could otherwise be called by the new settlement code in the wrong mode. Keep identity validation separate from ordinary-mutation permission: matching settle needs to validate a deliberately locked position.

### Borrowing

| Entry/path | Required treatment while active |
| --- | --- |
| `deposit_collateral`, `donate_collateral`, `withdraw_collateral` | Reject ordinary mutation before transfers and contribution updates. Donations cannot change the bound original position through an unrelated instruction. Direct token donations to custody confer no new accounting rights. |
| `borrow`, `repay` | Reject ordinary mutation; preserve the original share/principal obligation and referral/contribution state. |
| `withdraw_all_collateral` | Explicit idle check before its debt-free test or transfer. This path uses unconditional Anchor closure and does not rely on an open-debt guard. `BorrowPosition::is_empty()` must also exclude an active session. |
| Existing start/fill/backstop auction instructions | During development, enforce idle checks; the final auction replacement must remove their obsolete public API/state rather than leave a second unguarded liquidation route. |
| `Liquidation::apply`, `settle_internal_liquidation` and new flash settlement transitions | Require the appropriate idle or bound-session mode explicitly. The temporary custody release is never grounds for an unrelated debt write-off. |

### Leverage

| Entry/path | Required treatment while active |
| --- | --- |
| `open_leverage` / account initialization | Cannot overwrite/reinitialize an active or existing nonempty position. Verify same-transaction close/reopen behavior and position incarnation binding. |
| `increase_leverage`, `decrease_leverage`, `add_leverage_margin`, `repay_leverage`, `remove_leverage_margin` | Require idle before mutation. `repay_leverage` shares the add-margin handler and must not be missed because it does not sell collateral. |
| `close_leverage`, `delegated_close_leverage`, existing full liquidation | Require idle for ordinary paths. Preserve owner-owned payouts and reject callbacks that try to erase or move the bound obligation. Partial flash settlement must not inherit unconditional account closure. |
| `withdraw_repaid_leverage` | Explicit idle check before its zero-debt check, transfer, and unconditional account closure. A zero-collateral implicit lock would not reliably cover this lifecycle. |
| Create/update/close delegation and delegate protection/order execution | Freeze position-linked authorization changes while active or prove a specifically permitted change cannot invalidate the session. Dusk must enforce the guard at its own CPI entrypoints; checks only in the delegate program are insufficient. |

Read-only previews may remain available, but should expose an active session as non-actionable or reject a request for an executable position operation. They must not report a normal executable close based on temporarily released custody. Market/other-position operations require shared-accounting coverage even when they do not mutate the locked position.

### Shared-market paths between endpoints

| Entry/path | Required treatment |
| --- | --- |
| `execute_parameter_proposal` | Remains composable. Bind payment, discount, fee treatment, reference context and settlement mode at begin. A parameter execution cannot retroactively soften the obligation; revalidate independent current-state safety constraints at settle and preserve outstanding reservations. |
| `swap`, other positions' borrowing/leverage/repayment/liquidation | Remain composable with ordinary health and accounting checks. Outstanding shares/exposure/custody reservations stay in force; a separate liquidation cannot consume this position's collateral or insurance reservation. No general flash health waiver. |
| `remove_liquidity`, `withdraw_single_sided`, `harvest` | Remain composable with additional accounting checks. Do not count in-flight collateral or unpaid obligations as free backing; preserve required physical cash and reservations. Test actual withdrawal/fee accounting, including full LP exit during a session. |
| `rescue_hlp`, `close_insolvent_hlp`, `settle_protocol_auction` | Account for shared reserve, debt, fee and hLP effects. Bound reference terms do not permit using stale physical cash or skipping post-state invariants. |
| PR #48 native collateral entry/close and margin paths | Reconcile its new instruction/state variants with the same lock, exposure and debt-free cleanup inventory before combining runtime implementations. |

Each path needs instruction-level coverage with the session active, including failed transaction rollback. A complete inventory is a requirement, not evidence that the tests already exist.

## Required transaction regressions

| Case | Required outcome |
| --- | --- |
| Missing/wrong/later-program finish; wrong accounts/index/discriminator; CPI endpoint | Begin rejects or the transaction rolls back with unchanged custody, debt, exposure, and no surviving lock. |
| Duplicate begin/finish; cross-position/session replay; account close/reopen | No second payout or obligation reset; an old finish cannot satisfy a new session. |
| Intervening repayment, collateral withdrawal/donation, delegation mutation, ordinary liquidation or write-off | Guard rejects; original debt and collateral obligation remains bound. Include debt-free cleanup paths explicitly. |
| Valid partial and final routes, including a Dusk swap between endpoints | Actual reserve credit governs repayment; a partial account survives with health improved toward the target after all fees; full settlement completes insurance/write-off and clears the debt leg; owner residual and closure are correct; no stranded lock. |
| LP withdrawal, parameter execution, same-market reborrowing and independent liquidation between endpoints | Operations either preserve all original session obligations and ordinary accounting checks or revert atomically. Flashed collateral cannot be counted twice, and a new debt leg cannot erase the original payment. |
| Token-fee/zero-credit/donation/account-alias/insufficient-output cases | Transfer boundaries conserve actual debits/credits; preexisting funds do not satisfy a new obligation; failed settlement rolls back the complete transaction. |

Unit tests of a standalone lock struct are insufficient. These cases need instruction-level execution against real position, session, token, and market accounts, with the relevant intervening instructions. The historical exploit PoC supplied by the user must not be executed.
