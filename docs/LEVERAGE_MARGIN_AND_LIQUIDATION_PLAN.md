# Dusk leverage margins and liquidation: implementation handoff

Status: implementation plan; economic parameters require calibration.

**2026-10-05 update:** read [the current decision record](LIQUIDATION_DECISIONS.md)
first. All eight product questions are answered there. It supersedes the older
timer, program-paid reward, surplus-return, eligibility and margin alternatives
below. Implementation belongs in #45, including #48's native-collateral feature.
The remainder retains the design history and calibration requirements; conflicting
proposals are not authorization to implement the previous policy.

Latest revision, 2026-10-02: the [emergency policy](EMERGENCY_LIQUIDATION_POLICY.md) now specifies timer resets, sampled confirmation, terminal disposal when historical price bounds fail, and the remaining physical-execution/keeper-cost limits. It supersedes the earlier observation-count fallback proposal in section 10.6. Read the [new comparison](calibration/EMERGENCY_MARGIN_RESULTS.md) alongside the [interpretation](calibration/README.md); no runtime parameters have been selected. PR #48 is now also open (see emergency policy section 6).

Prepared: 2026-10-02. This document records the design discussion and gives a coding agent a sequence of work. Creating this document did not change protocol code, approve a deployment, or select final risk parameters.

Implementation authorization, updated 2026-10-02: the user selected the full plan and requested adding its implementation to existing PR #45. Borrowing auctions are to be replaced with immediate flash liquidations. Discuss and agree liquidation pricing before implementing that replacement. The user selected a bounded progressive keeper reward that continues for insolvent positions, with surplus proceeds benefiting the position, and authorized an emergency full Dusk AMM liquidation below the ordinary reference-price floor under a protocol-defined trigger. The user explicitly accepts competitive keeper racing as a design assumption. Full liquidation must finish the insurance/write-off waterfall atomically. Calibrate the reward, trigger, and margin settings before selecting their numeric values.

## 1. Objective and first action

Replace the standalone 2% leverage unwind-impact rejection with a coherent margin system that permits larger positions when they have enough equity and can be liquidated acceptably:

1. Increase initial margin gradually with total exposure in the same direction.
2. Use progressive position-size bands for maintenance margin.
3. Prefer partial leverage liquidation that restores health, with full liquidation when partial recovery is infeasible or uneconomic.
4. Plan flash liquidation settlement for both borrowing and leverage so liquidators can source debt tokens through external execution, while Dusk controls eligibility, collateral release, repayment, and losses.
5. Preserve the ongoing audit remedies, transfer-fee support, accounting invariants, and ordinary Dusk AMM execution flows.

**Start by reconciling the implementation base with PR #45 and writing the liquidity-denominator and valuation specification. Then build the calibration harness. Do not begin by deleting the 2% check or installing the illustrative constants as final defaults.**

The user prefers one coordinated PR for related implementation and remediation. Do not create several overlapping implementation PRs.

## 2. Decisions, proposals, and unresolved choices

### Confirmed direction

| Topic | Direction agreed in the discussion |
| --- | --- |
| Exposure restriction | Prefer dynamic initial margin and size-dependent maintenance over the proposed hard exposure cap at 50% of TVL. |
| Initial margin | Total same-direction market exposure affects requirements for new risk. |
| Maintenance margin | Individual position size affects the liquidation threshold. Other traders opening positions must not directly increase an existing position's maintenance requirement. |
| Liquidation | Partial liquidation first for leverage; full liquidation for insolvency, uneconomic leftovers, or no feasible partial recovery. |
| Borrowing auctions | Replace them with immediate flash liquidations; settle the pricing design before implementation. |
| Keeper incentive | Bounded progressive program-paid reward, including for insolvent cleanup. Measured execution surplus reduces surviving debt; after full repayment, owner residual is paid to an owner-owned account. Numeric curve, cap, and any time component require calibration. |
| Keeper competition | Treat permissionless keepers racing for profitable fills as an explicit accepted design assumption. Evaluate costs and attainable rewards under competition; do not keep treating hypothetical absence of competition as a reason to reject the chosen mechanism. |
| Emergency execution | Allow full liquidation through Dusk's own AMM below the ordinary protected-execution floor under a protocol-defined emergency condition. Complete recovery, eligible insurance draw, and remaining write-off atomically; leave no unresolved debt for the fully settled debt leg. |
| Behavior | Smooth, understandable changes; avoid sudden tier cliffs and unnecessarily aggressive increases. |

### Working proposals, not finalized product decisions

| Topic | Proposed implementation direction |
| --- | --- |
| Routine execution | Keep normal leverage open/increase/close execution on Dusk. The user raised this as a way to preserve AMM fee income while allowing external execution for liquidations. Do not expand scope to general external user settlement without confirming it. |
| Maintenance bands | Compare fixed amounts, depth-relative gentle/earlier schedules, and a separately unapproved stack-at-entry alternative. Snapshot depth is the candidate for keeping other traders/LP withdrawals from directly repricing existing MM. Concentrated-depth treatment and risk-increase refresh rules remain unresolved. |
| Entry buffer | Initial margin at least maintenance plus 3 percentage points. |
| Recovery buffer | Partial liquidation targets maintenance plus 2 percentage points, calculated on the remaining position. |
| Flash execution | Use a transaction-level begin/settle design as the candidate architecture. Route construction can follow later; the protocol must enforce settlement independently of Jupiter. |

### Decisions the coding agent must make concrete for the user

Present evidence and a recommendation for these choices; continue independent math, accounting, and test work while clarification is pending.

| Decision | What must be presented before selecting the behavior |
| --- | --- |
| Reference liquidity | Exact formula, units, smoothing, cash bounds, and manipulation behavior. Show what a "$100k market" means under that formula. |
| Parameter calibration | Recommended slope, bands, buffers, and modeled losses for thin liquidity, delayed execution, and simultaneous liquidations. The percentages below are inputs to compare. |
| Changes in liquidity | Whether LP withdrawals change only entry requirements or also existing maintenance bands; consequences for liquidations and withdrawal liveness. |
| Liquidation eligibility | Preserve the existing reference/executable-health combination unless a change is explicitly chosen. Explain any proposed change needed for external liquidation. |
| Liquidation pricing and loss policy | Auction replacement, progressive incentives including insolvent cleanup, surplus allocation, competitive keepers, atomic insurance/write-off, and a conditional full AMM emergency fallback are confirmed. Select the protected floor, reward curve/cap, and exact emergency condition. Finding #287652 remains an economic design risk; atomic settlement alone does not fix adverse AMM execution. |

Do not interpret an implementation detail as permission to change one of these product decisions. Equally, do not keep asking about the confirmed decisions above.

## 3. Repository and PR baseline

Verified on 2026-10-02:

| Item | Observed state |
| --- | --- |
| Remote `main` | `e8f12ea78922d305a2888b895b7d8fbe0994bc01`, merge of PR #43. |
| Only open PR returned by GitHub | [#45: fix: consolidate Dusk audit and transfer-fee changes](https://github.com/omnipair/dusk/pull/45). |
| PR #45 head | `5b123acf30574473d4a9346fbd1195d6d6cf1733`. |
| Current local checkout | `feat/permissionless-yield-harvest`, with unrelated modified and untracked files. It is not the implementation base for this handoff. |

PR #45 overlaps leverage transitions, liquidation instructions, state, previews, SDK interfaces, delegate settlement, and tests. It carries owner-scoped position addressing, owner-owned delegated payouts, fee splitting, freeze/transfer-hook admission checks, mutable transfer-fee support, and other audit fixes. Its body describes #287652 as still unresolved.

Recheck remote `main`, open PRs, and the PR head before implementation. The user explicitly chose to add the full implementation to #45. Use its attached checkout on `fix/benchmark-replay-owner`. Work on that branch while preserving the consolidated changes. Do not merge or rewrite its history merely to begin this task. If it merges or changes concurrently, reconcile before pushing further work.

Read repository `AGENTS.md` and its referenced branch-naming preferences before creating or changing branch/PR metadata. Use the required `feat/` prefix for new feature branches. Preserve all unrelated local changes; use a suitable isolated checkout for implementation. There are no production markets or legacy-account migration requirements unless the user introduces them.

## 4. Current behavior to preserve or deliberately replace

At the verified main commit, `programs/dusk/src/constants.rs` contains a 20x leverage circuit breaker, a 2% standalone maximum unwind impact, 10% initial margin, and 7% maintenance.

`programs/dusk/src/transitions/leverage.rs` has the core entry, increase, decrease, close, partial close, repayment, margin withdrawal, health, and liquidation logic. Initial admission uses both an executable Dusk closeout assessment and an EMA/reference assessment. The 2% rejection is an additional check inside `require_initial_leverage_health`.

Main's leverage liquidation closes the entire position. Its normal eligibility requires both executable closeout health and EMA/reference health to be at or below maintenance. This is not a spot-only eligibility rule.

At the inspected PR #45 head, fee-aware helpers include `LeverageCollateralFee`, `leverage_closeout_value_at_time_with_fee`, and `ema_leverage_margin_bps_for_credit`. Current and scheduled fees affect risk. The final notice epoch before a scheduled fee activates can make a position eligible independently of the normal two-condition path. Preserve that remedy and the zero-credit settlement path when refactoring. Read [PR #45's transfer-fee policy](https://github.com/omnipair/dusk/blob/5b123acf30574473d4a9346fbd1195d6d6cf1733/docs/mutable-transfer-fees.md).

`partial_close_leverage` proportionally closes debt/collateral and can return equity to the owner. That is not the required recovery liquidation: recovery should use sale proceeds to reduce debt while retaining remaining equity in the position. Reuse audited repayment/accounting primitives, not the owner-payout semantics of proportional partial close.

The current leverage liquidation instruction has unconditional position-account closure through its account constraints. Supporting partial liquidation therefore requires instruction/account-lifecycle changes, not just changing the amount sold.

## 5. Define exposure and liquidity before selecting constants

### 5.1 Units and valuation

Use explicit names and separate the following quantities:

| Symbol | Meaning |
| --- | --- |
| `N_i` | Current gross reference-valued collateral exposure of leverage position `i`; not its original margin deposit, historical entry value, or requested multiplier. |
| `O_s` | Sum of active leverage `N_i` on direction `s`, including the proposed final state of a new/increased position. |
| `L_s` | Conservative reference liquidity for that direction, denominated in the same unit as `O_s`. Exact construction is a calibration decision. |
| `V_ref` | Position collateral value under the canonical risk reference, adjusted for the applicable exit-transfer-fee policy. |
| `V_exec` | Debt-token value actually recoverable under the specified Dusk closeout quote, including applicable execution costs and transfer legs. |
| `D_i` | Current indexed debt, including accrued interest, converted consistently with the valuation being compared. |

Use gross exposure for the crowding numerator so imposing a punitive transfer fee cannot mechanically erase outstanding exposure. Apply fee haircuts to recoverability and health separately. Confirm this representation against the actual long and short accounting before implementation.

Dusk pairs need not contain a stablecoin. Dollar amounts in this document are illustrative. Specify normalized token units and the reference conversion for both directions, including base debt/quote collateral. Do not introduce an assumed USD oracle or silently use historical `open_notional` for current risk.

Maintain directions separately. Opposing positions are not automatically a safe offset for the cash and collateral needed to liquidate either side. Public borrowing and hLP obligations must still be included in shared cash/health constraints even if they are not in the leverage-only `O_s` counter.

### 5.2 A concrete $100k example

A balanced, plain constant-product pool containing $50k USDC and $50k META has $100k total pool value. At the instant before an unwind:

| Reference value being sold | Fraction of $100k total pool value | Fraction of $50k collateral-side reserve | Debt-token output before fees | Shortfall from starting reference value |
| --- | --- | --- | --- | --- |
| $10,000 | 10% | 20% | $8,333.33 | 16.67% |
| $25,000 | 25% | 50% | $16,666.67 | 33.33% |
| $50,000 | 50% | 100% | $25,000.00 | 50.00% |

These calculations use `output = 50,000 * sale_reference_value / (50,000 + sale_reference_value)`. They assume fixed starting reserves and exclude fees. A real entry changes the pool, and Dusk's concentrated curve can differ substantially; the actual simulation must execute the opening trade and subsequent state transitions.

This example is why the earlier 7–10% maintenance proposal cannot be called calibrated for a $100k pool. Also, the earlier "$100k reference liquidity" was a hypothetical denominator, not a verified on-chain measure.

### 5.3 Denominator candidates to compare

Evaluate at least these two constructions in the calibration harness:

| Candidate | Benefit | Limitation to quantify |
| --- | --- | --- |
| Cash-backed balanced-equivalent liquidity: twice the smaller reference-valued side of executable reserve cash | Simple comparison with the $100k balanced-pool example; excludes fictitious additional cash. | May understate concentrated depth, reacts to inventory imbalance, and may become restrictive after a trade or withdrawal. |
| Cash-bounded executable-depth measure derived from Dusk's curve/risk state | Better represents liquidation execution in concentrated markets. | Needs a precise price-impact/stress convention and protection against temporary depth or price manipulation. |

For the first candidate, use the protocol's definition of executable reserve cash, excluding liabilities and segregated custody. Do not count collateral already held for positions, insurance, fees owed, debt receivables, LP claims on the same cash, or hLP synthetic depth as additional independent cash. For the second candidate, virtual depth may describe execution shape but cannot override physical settlement constraints.

Compare live depth with a conservative observation policy, such as recognizing increases gradually and losses of available cash immediately. This is a candidate to assess, not a preselected smoothing window. A same-transaction liquidity donation followed by withdrawal must not cheaply buy permanent leveraged capacity.

Required denominator behavior:

1. Use coherent pre/post-operation observations; an entry cannot manufacture a larger denominator through its own price movement or double-counted borrowed cash.
2. Define bounded behavior at zero depth, stale observations, missing initialization, and extreme price/decimal values. Avoid division by zero and unlimited leverage on error.
3. Require real cash and existing market health independently of the exposure ratio. A denominator is not a promise that every position can exit simultaneously.
4. Do not credit unverifiable off-chain aggregator quotes as durable on-chain capacity. Model external liquidity disappearing during stress.
5. Specify LP withdrawal interactions. Smoothing must not hide lost executable cash; introducing new withdrawal restrictions is a product decision.

## 6. Candidate initial-margin curve

The following reproduces the gentle curve discussed. Implement it first as a parameterized simulation candidate.

Let `u_bps = ceil(10,000 * O_s_after / L_s_after)` with checked wide arithmetic and the observation rules selected in Section 5. A candidate curve is:

```text
market_IM_bps = min(
    10,000,
    1,000 + ceil(max(0, u_bps - 2,000) / 10)
)

required_IM_bps = max(
    market_IM_bps,
    effective_MM_bps(N_i_after) + entry_buffer_bps
)

candidate entry_buffer_bps = 300
```

Validate configuration so the required margin cannot exceed 100% through an invalid maintenance/buffer combination. At 100% required equity, no positive-debt position passes; this is a mathematical endpoint, not approval for the large exposure ratios leading to it.

| Exposure/reference liquidity | Candidate market IM |
| --- | --- |
| 0–20% | 10% |
| 40% | 12% |
| 50% | 13% |
| 80% | 16% |
| 100% | 18% |

One percentage point means 100 basis points. At a $100k denominator, exposure rising from $20k to $21k raises market IM from 10% to 10.1%. It must not jump to the next whole-percent tier.

Apply the resulting initial requirement to the canonical reference and executable health checks, with consistent amount/rate rounding. Preserve the fact that actual execution, fees, or other constraints may require more wallet equity than the simple schedule suggests. A nominal `1 / IM` figure is not a guaranteed attainable leverage multiplier.

### Operation behavior

| Operation | Intended margin behavior |
| --- | --- |
| Open/increase | Evaluate the resulting whole position and resulting aggregate exposure. Require initial health and all existing cash/market guards. |
| Remove margin or another risk-increasing mutation | Require the applicable initial-health checks on the resulting state. |
| Add collateral/repay debt | Permit genuine risk reduction without requiring an unhealthy position to become fully initial-margin compliant in one step. Preserve accounting checks. |
| Owner reduction/close | Use appropriate remaining-position health rules; do not require current crowded-market IM merely to reduce risk. Full repayment/close must not be blocked by entry crowding. |
| Third-party opens elsewhere | Affect future entry requirements, not this position's maintenance rate directly. |

The curve is recomputed from state, not accumulated as a time-based charge. It falls as the relevant exposure ratio falls. Interest still accrues and reduces equity separately. If liquidity falls, entry requirements can rise even without a new position.

There is no approved replacement hard cap at 50% of TVL. Do not silently add one. Conversely, do not flatten the curve at 18% or another low rate and claim unlimited further exposure is supported.

## 7. Candidate progressive maintenance schedule

For the illustrative market, test these fixed band amounts:

```text
maintenance_amount(N) =
    7%  * min(N, 10,000)
  + 8%  * min(max(N - 10,000, 0), 20,000)
  + 10% * max(N - 30,000, 0)

effective_MM_bps(N) = ceil(10,000 * maintenance_amount(N) / N)
```

Handle `N = 0` explicitly. Implement the amount formula with checked integer arithmetic and conservative rounding; document any atom-scale discontinuity. Do not independently round every component in ways that create avoidable large boundary errors.

| Current position reference value | Maintenance amount | Effective rate |
| --- | --- | --- |
| $10,000 | $700 | 7.00% |
| $25,000 | $1,900 | 7.60% |
| $30,000 | $2,300 | 7.67% |
| $50,000 | $4,300 | 8.60% |
| $100,000 | $9,300 | 9.30% |

The higher rate applies only to the portion in that band. This avoids repricing the entire position abruptly at a boundary. The effective rate approaches 10% in this candidate. That bounded rate does not establish safety for arbitrarily large positions.

Compute band membership from current reference-valued size, not entry size. Price changes and collateral changes can move a position through bands. For now the dollar-like band amounts are fixed simulation parameters, not permission to rescale existing positions' bands whenever liquidity changes.

Keep the size measure and the health measure explicit: `N_i` determines the maintenance rate; fee-aware `V_ref` and `V_exec` determine equity and recoverability. Do not compare an amount expressed in one asset with debt expressed in the other.

Position splitting lowers effective progressive maintenance. The aggregate initial-margin curve limits additional crowding, but does not eliminate the advantage of splitting. Model many wallets, many positions, and existing positions established before later crowding. Owner aggregation alone does not solve a many-wallet strategy. Present the residual risk rather than claiming the design is splitting-proof.

## 8. Shared risk calculations and state accounting

Create a small deterministic risk module reused by instruction transitions and previews. Suggested responsibilities, with names adapted to repository conventions:

| Helper | Responsibility |
| --- | --- |
| `position_reference_exposure` | Current normalized size and direction. |
| `market_reference_liquidity` | Selected denominator, validity, observation provenance, and bounds. |
| `initial_margin_requirement` | Post-operation aggregate curve plus entry buffer. |
| `maintenance_margin_requirement` | Progressive amount and effective rate. |
| `liquidation_recovery_requirement` | Remaining-position maintenance plus recovery buffer. |

Avoid on-chain loops over all positions. Store aggregate quantities that can be repriced from a single current observation. Prefer collateral quantities grouped by collateral/debt direction over a cached dollar sum that becomes stale when the price changes. Define whether an exposure remains active after debt is fully repaid but before collateral is withdrawn; recommended interpretation is leveraged exposure only while debt remains, with collateral custody tracked separately.

Every position lifecycle path must update aggregates exactly once: opening/increasing, adding or withdrawing collateral, reductions, owner partial/full close, repayment that extinguishes debt, partial/full liquidation, fee-driven zero-credit closure, and flash begin/settle. Audit the actual set of instruction paths rather than relying only on this list.

Adding collateral must remain permitted as risk reduction even if the gross exposure counter rises. Separate accounting of size from admission policy.

After each completed transaction, aggregate exposure quantities must equal the sum over live eligible positions. During a flash sequence, released collateral and unextinguished debt remain accounted for as pending exposure; they cannot disappear from global health or become spendable free liquidity before settlement.

Use checked arithmetic with documented saturation only where economically intended. Centralize rounding conventions: debt and required equity round conservatively; available value and realizable credit do not round upward to create capacity. Handle interest checkpoints before health decisions consistently.

## 9. Partial leverage liquidation

### 9.1 Eligibility and target

Replace constant maintenance comparisons with the approved size-dependent rate throughout normal eligibility, scheduled-fee eligibility, previews, and protection/order interactions. Preserve main/PR #45's eligibility semantics unless the user approves a different policy. A bad external route quote cannot itself make a healthy position liquidatable.

For a solvent eligible position, choose a collateral sale that restores:

```text
remaining_health >= effective_MM(remaining_reference_size) + recovery_buffer
candidate recovery_buffer = 200 bps
```

Evaluate both required reference and executable remaining-health conditions under the selected policy. Do not force recovery to current market IM; crowding should not turn a modest repair into unnecessary full liquidation.

Apply realized proceeds to obligations after explicitly permitted liquidation costs. Solvent partial repayments retain proportional principal/interest treatment; the new shortfall model uses principal-first sale recovery and principal-only insurance, pending selection for runtime. Keep owner equity inside the surviving position. Return owner residual only in a final settlement or another explicitly authorized owner withdrawal.

### 9.2 Arithmetic sanity example

For a simplified first-band position with $10,000 collateral value, $9,300 debt, and $700 equity, maintenance is 7%. A 9% recovery target can be reached by selling $2,222.22 and applying all proceeds to debt if execution is lossless:

```text
remaining collateral value = 7,777.78
remaining debt             = 7,077.78
remaining equity           =   700.00
remaining equity fraction  = 9%
```

With constant total execution loss `c` per dollar of reference value sold, constant target `r`, initial value `V`, and equity `E`, a useful sanity formula is:

```text
sale_reference_value = (r * V - E) / (r - c)
```

It applies only when the assumptions hold and the result is feasible. At `c = 2%`, this example needs approximately $2,857.14 sold to net $2,800 debt repayment. If costs prevent recovery, selling more is not automatically a cure. Real Dusk execution is nonlinear, transfer fees may be capped, maintenance bands change with the remainder, and debt-share rounding matters.

### 9.3 Solver and settlement requirements

1. Construct candidate sale sizes using canonical quotes and fee policy. Bound runtime and compute. Prove monotonicity before using binary search; otherwise evaluate tier boundaries and use a solver appropriate to the actual shape.
2. Prefer the smallest feasible repair subject to an explicit dust/efficiency rule and bounded tolerance. Do not let a caller select excessive collateral seizure just because it passes a lower-bound test.
3. Validate actual collateral debit and actual debt-vault credit, settle indexed debt, then recompute remaining health from the committed economic state.
4. Keep a surviving account and custody allocation for a partial result. Close accounts, clear delegations/orders as appropriate, and return rent only for final closure.
5. Fall back to full liquidation when the approved insolvency/dust/no-feasible-repair rules require it. Prevent uneconomic tiny repeated liquidations and repeated extraction of a fixed bounty.

The recovery buffer, minimum efficient repayment, incentive basis, and full-liquidation fallback must be calibrated together. Do not introduce a cooldown that blocks urgently needed further derisking without an explicit decision.

## 10. Flash liquidation settlement

This is the execution architecture for both borrow and leverage liquidation. Auction replacement is authorized. Any change to eligibility, protected execution terms, or the emergency condition must be explicit.

### 10.1 Suggested transaction shape

```text
begin_liquidation
    -> release a bounded collateral amount under Dusk's terms
    -> execute one or more swaps / source repayment assets
    -> settle_liquidation
    -> finalize debt, remaining collateral, rewards, exposure, and events
```

If settlement fails, the entire transaction must revert. No collateral release may survive without its paired settlement. Instruction names are placeholders; reuse existing primitives where appropriate.

The keeper may use Dusk, an external AMM, its own inventory, or a route assembled by an aggregator. Dusk must validate economic terms from accounts and balance changes, without trusting a route's reported return value. Off-chain routing seeks good execution; accepting a transaction on-chain requires enforceable settlement terms and does not prove a globally best price.

### 10.2 Begin/settle contract

| Stage | Required contract |
| --- | --- |
| Begin | Accrue/checkpoint required state; validate eligibility; derive bounded seizure and settlement obligations; verify an exact compatible later settle instruction; record the session; release only the permitted collateral. |
| Between instructions | Maintain pending exposure and unavailable-cash accounting. Guard all affected position mutation paths. Do not let temporary custody changes improve borrow/withdrawal capacity. |
| Settle | Bind to the exact session; reload token accounts; measure repayment credit; enforce repayment, fee, and remaining-health terms; update all accounting exactly once. |
| End | Clear the active session and pending state; emit authoritative events; preserve or close the position according to the actual result. Failure rolls back the whole transaction. |

Use explicit session/lock state instead of overloading a zero collateral balance or a hidden bit in an unrelated field. Bind position, market, debt/collateral mints, relevant custody and repayment accounts, caller authority where required, instruction identity/index, permitted amount, and session identity. Reject duplicate begin, duplicate settle, wrong pair/order, unmatched account substitution, and cross-position/cross-market replay.

A position lock alone is insufficient: other instructions touching shared market cash during the transaction must respect in-flight inventory and debt. If a market-wide guard is needed, explain its impact on using Dusk itself inside the route before choosing it.

Use canonical instruction introspection and verify actual runtime/CPI constraints. Do not copy old assumptions such as a fixed 40-account route budget or fixed CPI-depth limit without checking the chosen Solana environment. Specify whether the pair is top-level only and document the composability consequence.

### 10.3 Repayment terms and failure policy

The user chose to replace borrow auctions with immediate flash liquidation. Agree the reference price, bonus or reward model, and unfillable-position fallback before implementing the replacement. Then remove the obsolete auction timing/state/API paths coherently across the program, SDK, keeper interfaces, tests, and documentation. Preserve the audited health, actual-credit, debt, and loss-accounting remedies independently of the removed auction mechanism.

For leverage, use approved liquidation eligibility and partial-recovery sizing. Dusk must bound collateral released and enforce the associated net repayment and maximum incentive. Setting the keeper's minimum output to zero must not authorize arbitrary seizure or manufacture an insurance claim.

Separate the keeper's own execution tolerance from the protocol's repayment obligation. Existing keeper balances may be used only through the explicit authorized settlement input. An owner wallet must never silently fund a liquidation shortfall.

Preserve an explicit insolvency path. Sale proceeds may be less than the debt; a completed full liquidation must still resolve the entire liquidated debt leg. After the bounded reward and sale repayment, draw eligible insurance under the existing limits, then write off any remaining unpaid obligation using the existing loss accounting. Clear the corresponding shares, principal, contributions, and exposure in the same transaction. Do not leave a successfully finalized position with an unresolved "authorized shortfall." The keeper must meet the protocol's execution obligation; it need not personally fund the owner's credit loss. An arbitrary caller-chosen output must not manufacture an insurance claim. Do not report finding #287652 as remedied merely because execution became atomic or external routing became available.

Insurance draws, socialized losses, principal write-offs, interest recognition, and referral credit must be based on the approved loss waterfall and actual realized credits. A written-off receivable is not cash repayment or yield. Do not introduce a mandatory EMA output floor or change the liveness-versus-price-floor tradeoff without presenting it for decision.

### 10.4 Transfer fees and recipient guarantees

Retain PR #45's supported mint policies and scheduled-fee treatment. For every physical transfer, document gross debit, actual net credit, and who bears the fee. Internal bookkeeping movements are not automatically token transfers.

Cover collateral release, route inputs/outputs, debt repayment, insurance, keeper rewards, owner refunds, and custody cleanup independently. Transfer-fee caps mean splitting or combining transfers changes the result. Do not reuse a full-position fee estimate for several partial transfers, or deduct the same fee twice between SDK and program.

Preserve owner-owned residual payout accounts, net minimum guarantees, bounded fee gross-up, and best-effort protocol-fee handling from the consolidated remediation. Preserve required withheld-fee handling when token accounts are closed.

### 10.5 Lock enforcement and the marginfi precedent

In a report published on 2025-09-17, Asymmetric Research described a marginfi vulnerability: account migration could move liabilities from account A to B during a flash loan, leaving the final health check on A with empty balances. The flash-loan flag was not enforced by that migration path. The issue was privately disclosed and patched; the report states that no funds were lost. [Primary disclosure](https://blog.asymmetric.re/threat-contained-marginfi-flash-loan-vulnerability/).

For Dusk, this is a regression-design lesson. A later settle instruction being present does not prove it will enforce the original obligation. The session must preserve the identity and obligations of the position throughout the transaction.

The earlier attached flash-close sketch suggested setting `collateral_amount = 0` and relying on `require_open()` as an implicit lock. Do not adopt that shortcut. Zero collateral can describe multiple economic states, and a new repayment, liquidation, account-transfer, or cleanup path could omit that helper. In particular, a temporary collateral release must never become permission to write off the associated debt.

Use an explicit state transition with enforced permissions:

```text
Idle
  -> begin: record session and obligations; mark active before collateral release
ActiveFlashSession
  -> only the matching authorized settlement may finalize this position
Idle (repaired position) or Closed (final settlement)
```

Ordinary swaps may remain available when shared-market accounting permits them; they do not acquire permission to alter the session's position. Transaction failure reverts the session and transfers together. This is an in-transaction guard, not a multi-block user lock.

Required implementation and review checks:

1. Capture the settlement obligation, released gross amount, observed transfer credit, original debt shares, and position/session identity before release. Define any permitted adjustment explicitly. Settlement must not accept an obligation that became zero through an unrelated instruction.
2. Centralize session-state validation for position mutations. Maintain an instruction inventory showing that every current path checks it, including owner/delegate paths, debt write-off, final cleanup, and any account transfer/reinitialization feature that exists. Future mutation instructions must join this inventory.
3. Reject migration, owner/authority changes, unrelated close/reinitialize, another liquidation, and unauthorized debt/collateral mutation while active. An intentionally permitted operation needs an explicit state transition that preserves the session contract; an omitted check is not permission.
4. At settlement, require the active original session, a valid account lifecycle state, and measured repayment of the bound obligation. Checking only the current position's health, zero debt, or zero collateral is insufficient. Clear the session only after settlement accounting is validated.
5. Add local regression cases for attempts to move or erase liabilities between begin and settle, including an intervening write-off and close/reinitialize where those operations exist. Assert transaction rollback, unchanged debt/custody/exposure on failure, and no stranded active session. Do not run an external historical exploit PoC.

The explicit flag itself is not a security guarantee. Correctness comes from enforcing the session contract across every relevant path and retaining global in-flight cash/exposure constraints from Section 10.2. The implementation inventory is in [flash settlement contract](FLASH_LIQUIDATION_CONTRACT.md); it describes required behavior, not implemented protection.

### 10.6 Emergency fallback calibration and refined direction

**Revision:** [EMERGENCY_LIQUIDATION_POLICY.md](EMERGENCY_LIQUIDATION_POLICY.md) is the current draft flow. The paragraphs below retain the earlier decision history and native price-path evidence. Distress is confirmed with elapsed time and freshness, not instruction counts; a tiny payment that leaves reference health unhealthy cannot reset it. Historical bounds may be waived for the terminal internal sale, with remaining execution-price risk stated explicitly. The extra `internal_minimum` terminal flash alternative is modeled separately and awaits the user's decision.

The user approved a full internal AMM fallback when protected partial liquidation cannot recover enough, with a protocol-defined emergency condition. It may execute below the ordinary reference-price floor and must realize losses through the existing audited waterfall. This does not authorize arbitrary external routes to return less than their bound obligation and charge the difference to insurance.

The native [calibration report](calibration/README.md) now includes 24 entry-to-price-path histories and 144 threshold crossings. In a plain $100k initial-depth example with $5k debt, the slower declining path recovers all debt at a 20% reference trigger, but leaves approximately $515 unrecovered at 7% and $680 at 3%, including an illustrative 0.25% proceeds-funded reward. On the faster path even the 20% trigger leaves approximately $515 unrecovered. These are scenario results, not final thresholds, and no liquidation is executed between the counterfactual crossing observations.

The earlier binary timing question and fixed 0.25% reward proposal were superseded in discussion on 2026-10-02. Do not ask the user to reconfirm those stale choices. The revised direction, which the user generally endorsed, is:

1. Preserve reference-confirmed liquidatability and protected flash fills, including full insolvent settlement when its execution minimum is met. A full settlement realizes any remaining loss through insurance/write-off within the same transaction.
2. Model the emergency trigger using both reference health and the executable debt-repayment cushion after costs. A marginal-price/EMA gap alone is insufficient: execution can deteriorate through depth loss with no gap. Consider persistent executable distress or a separately critical reference level, together with infeasibility of a protected internal partial repair. Exact thresholds and confirmation mechanics remain unselected.
3. Keep incentives progressive and available for insolvent cleanup. Health-based escalation is the initial candidate; any time component, minimum cleanup bounty, reward basis, and cap still require calibration. Evaluate the total reward for partial versus full execution, including repeated partial fills. Do not add a separate reward jump merely for choosing the emergency mode.
4. Competitive permissionless keeper racing is an accepted economic assumption. Do not assume a single keeper can reserve an opportunity while waiting. Model early execution profitability and costs with that assumption. Tiny-position batching or a funded minimum bounty may still need a product choice because a capped percentage of very small proceeds can be below execution cost.

An inability to find a protected Dusk repair does not prove external liquidity was absent. A failed transaction or keeper assertion must not count as such proof. The proposed confirmation and critical-health conditions need native simulations before being presented as selected or proven safe.

Do not interpret the native leverage paths as borrowing calibration. Borrowing has collateral-factor/global-health accounting and can hold both debt directions; its eligibility and settlement need their own tests. Finalizing an emergency rule does not independently select maintenance bands, the protected floor, or transfer-fee treatment.

## 11. Calibration work and required evidence

Build a deterministic simulator using the actual Dusk quote/risk/accounting code where practical, plus independent arithmetic checks for simple cases. Do not rely exclusively on a separately reimplemented approximate AMM. Do not execute the previously supplied exploit PoC; the user explicitly requested that it only be read. New economic simulations and normal implementation tests are separate work.

### Scenario matrix

| Dimension | Minimum coverage |
| --- | --- |
| Liquidity scale | Approximately $25k, $100k, and $1m illustrative markets; quote-token units clearly identified. |
| Curve | Plain constant-product-equivalent case and supported concentrated configurations; include moving outside concentrated depth. |
| Positions | Single large position; many positions in one wallet; many wallets; long-only, short-only, and both directions; public-borrow and hLP obligations present. |
| Exposure | Candidate curve below its free band, through normal operating range, and into stressed high exposure; include repeated entries using borrowed cash recycled through Dusk. |
| Price path | Gradual trends, gaps, reversals, and different EMA lag states; simulate opening and closing, not only static reserve snapshots. |
| Liquidity loss | LP withdrawals, changing cash availability, and temporary depth additions removed after entry. |
| Execution | Dusk-only recovery, external depth available, external depth absent, split routing, failed attempts, and keeper delays. |
| Fees | Legacy tokens; current/scheduled Token-2022 fees; small caps; near-total fees; zero-credit liquidation; both transfer directions. |
| Liquidation sequence | Partial recovery, repeat recovery, concurrent distressed positions processed sequentially, dust, and unavoidable full closure. |

Compare the current 2% rule, the candidate gentle curve, and alternative slopes/bands using the same market paths. Include moderate and severe shocks without presenting the severe cases as zero-loss promises.

Report:

1. Admitted exposure, required wallet equity, actual effective leverage, and which constraint binds.
2. Reference health versus executable health, realized repayment, remaining position health, and recovery size.
3. Keeper incentive after all costs, failed-fill rate, time to debt recovery, and repeated liquidation behavior.
4. Insurance consumption, socialized principal losses, owner residuals, and impact on each affected LP class.
5. Dusk swap-fee income versus external execution costs and avoided losses; no assumption that arbitrage returns exported fee revenue.

Deliver a small calibration report with reproducible inputs, tables/plots, selected candidate parameters, rejected alternatives, and sensitivity ranges. Include a clear recommendation about the denominator and liquidity-withdrawal behavior. Ask the user to resolve only the material product/loss-tolerance decisions still open; do not claim a universal safe margin schedule for all permissionless assets.

## 12. SDK, previews, events, UI, indexer, and VOB

Risk calculations must have one authoritative on-chain implementation and consistent previews. Expose the inputs that explain a rejection instead of duplicating an approximate threshold in a frontend hint.

| Surface | Required follow-up |
| --- | --- |
| Leverage preview | Current/post-operation side exposure, reference liquidity and units, IM/MM amounts/rates, relevant fees, executable/reference health, and binding rejection reason. |
| Liquidation preview | Eligibility reason including scheduled fees, proposed collateral sale, net repayment, incentive, remaining health, partial/full outcome, and fallback reason. |
| SDK builders | Updated accounts/arguments and bounded flash begin/settle construction; route-independent settlement requirements. |
| Events/indexer | Record partial versus final liquidation, actual gross/net transfers, debt-share changes, exposure changes, reference context, insurance, and write-offs. Indexers must retain a position after a partial liquidation. |
| UI/VOB/keepers | Remove the old 2% hint only with the protocol replacement; display attainable leverage from previews; refresh IDL/account decoding; preserve swap gross/net conventions and avoid invented Dusk swap volume for external execution. |

Additive risk fields can still change serialized layouts and clients. Inspect which fields VOB and the indexer deserialize. Do not claim they are unaffected because the core swap curve was unchanged. External liquidation must not emit a Dusk `SwapExecuted` event for a trade that did not execute in Dusk.

A production Jupiter adapter, general owner flash close, new position-operation fee, and external routing for ordinary entries/exits are outside this implementation unless separately approved. Provide enough protocol and SDK support to test flash settlement without completing those integrations.

## 13. Implementation sequence and likely files

### Phase A — baseline and economic specification

Reconcile #45, inventory mutation paths and account layouts, specify units/denominator/observations, and implement the simulator. Produce the decision report before selecting runtime defaults or replacing admission checks.

### Phase B — risk helpers and aggregates

Implement parameter validation, progressive maintenance, aggregate exposure accounting, and shared previews. Exercise them in tests and simulation. Define behavior for invalid/zero liquidity and both debt directions. Avoid creating an unnecessary governance/configuration framework; propose the smallest supported configuration surface consistent with permissionless creation.

### Phase C — margin enforcement

Wire the approved schedule into opens, increases, risk-increasing withdrawals, and all remaining-health checks. Replace the standalone 2% rejection when the complete replacement and regression coverage are ready. Preserve the 20x circuit breaker and other existing constraints unless a change is separately justified and approved.

### Phase D — partial leverage liquidation

Implement bounded recovery sizing, partial settlement, account retention, and final-close fallback. Integrate transfer-fee rewards, owner payouts, debt/referral accounting, insurance, and scheduled-fee eligibility from #45. Use the same health helpers in execution and previews.

### Phase E — flash liquidation settlement and integration

Implement the approved begin/settle contract for leverage and borrowing, replacing borrow auctions with immediate settlement under the pricing policy agreed with the user. Verify shared-state behavior during intervening route instructions. Complete SDK, events, previews, and integration handoff.

These are phases within coordinated work, not instructions to open five PRs. If the user chooses a narrower initial delivery, record the explicit scope change.

| Area | Likely files or directories to inspect |
| --- | --- |
| Core leverage | `programs/dusk/src/transitions/leverage.rs`, `programs/dusk/src/instructions/leverage/`, `programs/dusk/src/state/leverage_position.rs`. |
| Borrow liquidation | `programs/dusk/src/transitions/lending/liquidation.rs`, `programs/dusk/src/instructions/lending/liquidation/`, `programs/dusk/src/state/borrow_position.rs`. |
| Shared risk/accounting | `programs/dusk/src/math/risk.rs`, `programs/dusk/src/state/market.rs`, `programs/dusk/src/transitions/liquidity/`, `programs/dusk/src/transitions/amm/`. |
| Public interface | `programs/dusk/src/lib.rs`, `constants.rs`, `errors.rs`, `events.rs`, `instructions/preview.rs`, and delegate instructions as needed. |
| Clients/tests | `packages/dusk-sdk/src/`, `programs/dusk/src/tests/`, `scripts/protocol-tests/scenarios/`, and the repository's LiteSVM harness. |

## 14. Verification matrix

Write tests for the economic and state invariants, not only helper outputs copied from the implementation.

| Category | Required assertions |
| --- | --- |
| Curve arithmetic | Continuity at the exposure free band and maintenance boundaries; one-atom cases; exact example results within defined rounding; overflow bounds; valid 100% endpoint. |
| Dynamic behavior | Opens raise the resulting aggregate; closes reduce it; prices revalue quantities; no time-based margin ratchet; other users' entries do not directly change existing maintenance. |
| State conservation | Aggregate counters match the positions after every operation sequence; interest and debt shares reconcile; rollback leaves no partial counter update; repaid-but-unwithdrawn collateral is handled consistently. |
| Admission | Fees, both health valuations, cash constraints, scheduled fees, and post-trade denominator all agree with previews. A split sequence cannot bypass aggregate accounting. |
| Partial liquidation | Minimal feasible repair within tolerance; remaining-size tier recomputed; no owner-equity payout during repair; correct account survival; debt and incentives cannot be settled twice. |
| Solver failure | Nonlinear/non-monotone recovery, inadequate proceeds, unavailable quotes, dust, and insolvency produce the approved fallback rather than endless failed partial attempts. |
| Flash atomicity | Missing/wrong/duplicate settle, nested begin, account substitution, unauthorized wallet debit, unrelated balance credits, and transaction failure cannot leak collateral or erase debt. |
| In-flight market state | Borrow, withdraw, repay, swap, fee claims, and other exposed mutation paths cannot use temporarily released inventory as unencumbered cash. A supported Dusk route still works. |
| Transfer fees | Capped fees on each leg, pending-epoch eligibility, gross-up, zero credit, keeper minimum, owner ownership, and cleanup preserve the consolidated fixes. |
| Loss allocation | Actual cash, insurance debit/net credit, principal write-off, interest, referrals, and LP effects reconcile; poor keeper execution does not create an unrestricted insurance claim. |
| Interfaces | SDK/on-chain preview parity, regenerated IDLs, partial-liquidation indexer replay, and VOB decoding/quote compatibility. |

Add meaningful property-based operation sequences where the repository supports them. Keep randomized seeds/reproducers for any discovered accounting failure. Measure compute, account count, transaction size, and lookup-table requirements using real SBF/LiteSVM instructions. Include nontrivial external-route execution through a fixture; do not present a fixture as proof of live Jupiter compatibility.

## 15. Required interface regeneration and pre-commit checks

Follow the current `AGENTS.md` and `.github/workflows/ci.yaml` if they change. At the time of writing, instruction accounts, arguments, return types, events, program IDs, and public Rust documentation count as interface changes.

After Dusk or leverage-delegate interface changes:

```sh
anchor build -p dusk
anchor build -p leverage_delegate
npm run prepare-idl --prefix packages/dusk-sdk
npm run check:dusk-sdk
```

Commit regenerated `packages/dusk-sdk/src/idl_v2.json`, `types_v2.ts`, `idl_delegate.json`, and `types_delegate.ts` with the implementation. A faucet interface change separately requires `anchor build -p faucet`; its IDL is not currently vendored into the SDK.

Run the full required local CI sequence before every commit:

```sh
cargo fmt --all -- --check
yarn check:hygiene
yarn check:code-shape
yarn check:clippy
cargo test -p dusk
cargo check -p dusk --lib --features production
cargo test -p dusk --lib --features production
cargo test -p leverage_delegate
cargo test -p faucet
yarn typecheck
yarn build:litesvm
npm run check:dusk-sdk
DUSK_REQUIRE_COMPLETE_CU_BASELINE=1 yarn test-litesvm:no-build --forbid-pending
```

Do not create a commit with a required check failing or claim a complete check when only a subset ran. Record the tested commit and any separate live-cluster follow-ups. Do not deploy or upgrade programs as part of this handoff without the applicable user authorization.

## 16. Completion criteria and handoff package

The implementation is ready for review when:

1. The denominator, units, parameter choices, liquidity behavior, and unresolved loss-policy choices are explicit and supported by the calibration report.
2. Dynamic initial margin, progressive maintenance, partial liquidation, and the agreed flash scope are implemented with conserved state and actual credited amounts.
3. The 2% rule has been replaced only alongside its intended replacement; audit remedies are preserved; no unsupported claim is made that atomic execution resolves #287652.
4. Previews, SDK/IDLs, events, integration notes, and required CI are current, with the exact tested revision identified.
5. One coordinated PR explains behavior changes, measured capacity improvements, liquidation outcomes, remaining accepted risks, and deployment/client follow-ups.

Include an operation-to-state-update matrix, example transactions, calibration inputs/results, a list of product decisions confirmed during implementation, and a concise indexer/webapp/keeper checklist. Keep unchosen options visibly separate from implemented behavior.

## 17. Sources and limitations

These sources support mechanisms, not the numerical safety of the proposed Dusk settings:

- [dYdX margin documentation](https://docs.dydx.xyz/concepts/trading/margin): market exposure can increase initial margin while maintenance remains separate. Dusk's size-dependent maintenance is an additional design choice.
- [Hyperliquid margin tiers](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/margin-tiers): continuity across size bands can be maintained through progressive requirements/maintenance deductions.
- [Hyperliquid liquidations](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/liquidations): partial liquidation is used in a different execution and risk system; its numerical thresholds and fallback behavior should not be copied into Dusk.
- [Uniswap AMM explanation](https://developers.uniswap.org/docs/get-started/concepts/how-uniswap-works): constant-product reserves explain the simple liquidity example. Dusk requires its own executable-curve model.
- [Verified Dusk main](https://github.com/omnipair/dusk/tree/e8f12ea78922d305a2888b895b7d8fbe0994bc01) and [inspected PR #45 revision](https://github.com/omnipair/dusk/tree/5b123acf30574473d4a9346fbd1195d6d6cf1733): implementation baseline and audit-remediation dependencies.

The interactive margin illustration used a $100k hypothetical reference denominator and candidate rates only. Its slider states, including very high exposure, do not demonstrate admissibility or solvency. This document intentionally requires those assumptions to be resolved before selecting protocol defaults.
