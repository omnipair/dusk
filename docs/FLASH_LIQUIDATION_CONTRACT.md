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

**2026-10-09 accepted design update:** ordinary purchase discount is the capped
maximum of health-based and elapsed-time discounts, with immediate fills and no
mandatory wait. The user selected 120 seconds to reach the 5% discount cap;
linear interpolation from 0.5% is the implementation starting point. A committed distress observation must survive a failed fill;
never rely on initializing the clock inside a transaction that then rolls back.
Useful partial fills do not reset ongoing distress; verified recovery ends the
episode. Bind the resulting payment at begin. A later time/health change cannot
reduce that session's obligation. Prevent stale episode reuse after recovery or
position recreation. Exact ramp/reset observation semantics still need tests.

The internal AMM emergency path is now partial-first: quote actual costs and
target the surviving position's MM +2 points, accepting useful smaller repairs.
The user selected access at 70% of applicable MM (4.9% EMA equity at 7% MM).
The emergency caller receives a progressive health/time reward capped at 1%
of actual internal sale proceeds, including insolvent sales. Exact interpolation
still needs specification and testing; this is separate from ordinary buyers'
profit on purchased collateral and does not guarantee economic dust cleanup.
Crossing the emergency boundary is not automatic full-close permission. Full
liquidation within that boundary is now approved when symmetric-EMA equity is
zero or negative, or the program verifies that no permitted partial liquidation
improves health after all costs. Preserve useful smaller repairs even when no
partial reaches the whole MM+2-point target. A keeper-selected failed quote,
restrictive request or failed transaction does not prove infeasibility; verify
the permitted sizes, including tier boundaries and integer rounding. The user
selected per-market minimum residual debt, denominated in each debt token's raw
units, and no fixed insurance-funded cleanup bonus. Configure amounts per debt
side; 5 USDC was an example. Resize partials to avoid subminimum leftovers when
health can still improve. Dust cannot independently bypass full-close or payment
permissions, or manufacture infeasibility by excluding useful partials. See the
current decision record for the proposed emergency reward interpolation, which
is still awaiting approval. Target-based insurance funding was accepted for development.
The user selected a provisional principal component of 5% of outstanding covered
borrower/leverage principal per debt token. The user selected full insurance
funding from the designated allocation through 75% of target, followed by a
linear decrease to zero at the target. The user selected existing market LP
yield distribution for the remainder, including hLP through its yLP holdings.
Markets may launch with zero insurance and optional pre-funding; no minimum
seed or insurance-funded launch gate is required. The user selected an
additional H/15 (approximately 6.67%) of current indexed hLP funding debt
in the same token, giving the combined target `ceil(P/20) + ceil(H/15)`.
P excludes hLP debt; H includes accrued funding interest. Selected
spending direction: up to 75% of eligible principal shortfall, capped by a shared
50% fund budget refreshing every 24 hours against the remaining balance. The
user confirmed preserving hLP's existing eligible-shortfall coverage, up to
100%, under that same shared budget. The 75% coverage rate applies to borrower
and leverage principal losses, not hLP's existing eligible funding shortfall.
Do not duplicate or reset the fund budget when switching claimant types.
Available cash and transfer-fee-adjusted credits still limit actual coverage;
preserve the existing hLP eligibility checks and residual-loss accounting.
The user also requires explicit protocol treasury revenue from solvent
borrowing and leverage liquidations. The user selected a 1% charge on net debt
repaid, split as 0.8% of repayment to insurance/LPs and 0.2% to protocol.
The user confirmed that the 0.8% allocation supplies insurance under the
target-based taper and sends its remainder to LPs; the protocol allocation is
separate. The 5% principal target and 75%-to-100% linear taper are selected for
calibration; the additional H/15 hLP target component is also selected. Optional top-ups
use the existing `fortify_market` instruction and actual net vault credits.
Credit the LP remainder under existing market yield ownership and checkpoint
rules, including hLP through its yLP holdings. Do not take a second protocol
share from the already allocated LP remainder or reapply an unrelated interest
or swap-fee percentage. Account for liquidation revenue distinctly in receipts
and events even if it reuses an existing yield-distribution mechanism.
Draws that reduce the funding ratio increase subsequent insurance contributions
under the same curve; no existing fund balance is paid out when the target falls.
Apply the 80/20 split to actually collected fees after any applicable cap. Bind the
charge and allocation at begin, include it in partial-recovery checks, cap full
solvent collection by actual repayment surplus after applicable caller reward,
and never fund the additional treasury fee through insurance or write-off.
Historical full-emergency/health-only experiments do not validate these changes.

**2026-10-10 fee-base decision:** quote net debt repayment R and a separate
bounded liquidation fee F calculated on R. The borrower funds F through
additional collateral; the buyer funds both R and F. The collateral quote must
also account for the ordinary buyer discount. Earlier examples deducting a fee
from the advertised debt repayment are superseded. Bind both component amounts
and fee destinations at begin and verify their actual spendable credits at
settlement, grossing up physical legs for transfer fees where needed. No fee
credit may also count as repayment R. Include all collateral seized and actual
costs in partial recovery; preserve the existing full-settlement surplus cap.
The old 0.2% total rate is superseded by 1%; the charge's 80/20 allocation means
R=1,000 requires 1,010 spendable payment, with 1,000 repaid, 8 to LPs/treasury
and 2 to protocol, before transfer-fee gross-ups. The user explicitly retained
the 0.5%-to-5% ordinary discount and emergency caller reward capped at 1% of
actual proceeds. These incentives are separate from the new 1% repayment fee.

**2026-10-07 routing clarification:** the user challenged the optional early
solvent full-close shortcut because it could bypass better execution and partial
repair. The 0–2% shortcut is withdrawn from the recommendation. Dusk remains an
allowed ordinary flash route under the same bound payment. The description of
the solvent candidate below is historical modeling context. The partial recovery
target is now confirmed as the surviving position's MM +2 percentage points.

## Implementation status, 2026-10-11

The current implementation and verification boundary are recorded in
[Liquidation decisions](LIQUIDATION_DECISIONS.md#current-implementation-checkpoint-2026-10-11).
The requirements and historical observations below explain the design; statements
that an instruction or policy remains unimplemented describe the earlier baseline.
The old runtime auction APIs are now removed. Historical auction state and native
replay routines remain for comparison, without public instruction entrypoints.

## Native execution and integration gates (historical baseline)

The [concentrated/hLP comparison](calibration/CONCENTRATED_CUSHION_RESULTS.md)
executes the native economic primitives, including both solvent repayment and
insurance/write-off. These primitives exist; their composition into the proposed
flash instructions is still required. The following gates remain material:

| Area | Evidence and required work |
| --- | --- |
| Spendable cash | A focused native test obtains a curve quote backed partly by debt receivables, then observes `InsufficientLiquidity` during actual spot preparation. Existing `Close`/`Liquidate` cash policies already net debt internally; this spot rejection does not prove those policies would fail. Validate the policy selected for the redesign, including hLP interest floors and physical payouts; a curve quote alone is insufficient. |
| hLP and recentering | The starting-debt mismatch is corrected: quotes retain recorded indexed hLP debt, funding is reserved once, and recenter funding projects the actual post-deployment state. All 576 native paths replay without the prior reserve-identity rejections. Seeded solvent and loss-taking anchors succeed in both debt directions. Preserve these regressions and the unchanged reconciliation guard when implementing the new instruction paths. |
| Loss accounting | Current native liquidation can clear debt, consume insurance and rebase the concentrated curve/hLP after principal loss. Its existing proportional realized-interest allocation is not the selected principal-first waterfall. Implement and test funded interest, canceled interest, principal-only insurance and fee/referral treatment; the new economic sweep freezes accrual and cannot validate these changes. |
| Concurrent state changes | An hLP transition is tied to specific curve revisions, LP supply and hLP shares/debt. A plan prepared at flash begin cannot simply be applied after allowed LP/parameter/other-position operations. Keep the original payment/debt obligation bound, then prepare valid settlement accounting against current shared state with reservations intact. |
| Instruction boundary | Native state tests do not exercise transfer-fee CPIs, session account lifecycle, token-account aliasing, exact begin/settle pairing, compute/account limits or concurrent instruction ordering. These remain required integration tests for the new entrypoints and previews. |

The ordinary hLP settlement-divergence check is used by single-sided entry and
withdrawal, not a blanket guard on every integrated swap or liquidation. Do not
infer that all hLP activity must pause for liquidation. Conversely, hLP funding
cash and reserve-identity constraints still apply to the native integrated path.

Same-market flash routing needs a separate cash-flow check: a spot swap must
physically pay its full output before later repayment, whereas integrated
close/liquidation can offset repaid principal. Do not promise identical capacity
from a raw spot-swap-plus-repay transaction. Design and test a session-bound
internal swap/repayment path with the same collateral authorization, ordinary
price floor and partial-recovery rules. Its accounting must distinguish actual
token credits from atomic swap/debt offsets, fund every physical interest,
fee/reward and owner payment, and preserve hLP backing and active-session
reservations. The generic spot cash guard remains enforced. This is an
implementation requirement, not evidence that such a path already exists.

Emergency permission uses symmetric EMA equity at or below 70% of applicable MM. This selected threshold still needs integrated calibration. There is no mandatory waiting period or time-only emergency unlock. The persistent distress clock affects the ordinary purchase incentive, reaching its cap after 120 seconds. The [older emergency policy](EMERGENCY_LIQUIDATION_POLICY.md) is historical calibration context. Internal price relaxation never permits an arbitrary external route to return less than its bound obligation.

The user also approved modeling an earlier **solvent internal AMM close** when
ordinary EMA eligibility holds and the full-sale repayment cushion is small but
nonnegative after bounded caller reward and applicable charges. This mode must
repay principal and accrued interest from actual execution, fund insurance only
from surplus, and return the remainder to the owner. It cannot draw insurance,
write off principal, or cancel interest; failed final coverage rolls back the
transaction. The cushion width is unselected. A negative quote does not itself
unlock the separate loss-taking mode. See the [comparison](calibration/REPAYMENT_CUSHION_RESULTS.md)
for gap risk, quote sensitivity and the cost of full close preceding partial repair.

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

Ordinary flash purchases use the selected progressive collateral discount, including insolvent cleanup under the bound-payment rule. For an internal emergency sale, pay the caller a progressive share of actual proceeds before recovery, capped at the selected 1%. Competitive permissionless keepers racing for profitable fills are an accepted design assumption. Use this assumption in economic calibration; the emergency reward uses the approved max(health, time) 0.30%–1% curve in
[the current checkpoint](LIQUIDATION_DECISIONS.md#current-implementation-checkpoint-2026-10-11). Per-market dust minimums without a fixed insurance-funded bonus are selected; configure amounts per debt token.

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
| Leverage funding | Optional collateral-funded entry/close removed on 2026-10-11. Debt-funded entry, margin paths and debt-free cleanup retain the same session guards. |

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
