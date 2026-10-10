# Emergency liquidation policy and calibration contract

**Historical proposal, superseded 2026-10-05:** use [the current decision record](LIQUIDATION_DECISIONS.md)
for implementation. Emergency permission now depends on critically low EMA equity
without a mandatory timer; the numeric threshold is unselected. This document and
its simulation results preserve the earlier comparison, including superseded
timers, reward model and dual eligibility gate. They do not validate the selected
policy.

Status: revised design for PR #45, 2026-10-02. No runtime change. The user requested this revision and comparison of margin alternatives. Numeric settings below are experiments, not selected program defaults.

## 1. Loss policy

The terminal objective is to dispose of collateral and finish the debt leg. A stale reference-price floor must not indefinitely prevent that operation. This follows the user's approved emergency AMM liquidation and atomic loss waterfall.

There are three execution modes:

| Mode | Permission and price protection | Result |
| --- | --- | --- |
| Protected flash | Position passes the existing reference AND executable eligibility checks. A protocol-sized partial restores both health measures after every cost; otherwise a permitted full fill satisfies the protocol's net repayment floor. | Partial retains the position; full clears the debt leg. An arbitrary route never receives an exception to its bound obligation. |
| Guarded internal full sale | No feasible protected partial; reference distress has aged sufficiently, or reference health is critical. A protocol-computed earlier quote bound still passes. | Dusk executes the actual internal swap, with bounded incentive and the loss waterfall. |
| Terminal internal full sale | Position remains eligible and reference health is critical OR the maximum confirmed distress age has elapsed; no feasible protected partial. Earlier quote/EMA bounds may fail. | Dusk executes its own current AMM quote in the same instruction and completes insurance/write-off. Historical price bounds no longer veto execution. |

Protected liquidation is available immediately. The timer gates only fallback. At terminal eligibility, a protected partial that actually meets its repayment and remaining-health obligations may still execute. A keeper may not simply assert that routes failed, select a larger seizure, waive a flash repayment obligation, or submit an arbitrary external output as a loss claim.

The internal quote and execution must bind the same prepared curve, fee, collateral amount, debt accounting and cash state, with no untrusted route between them. This prevents a caller substituting a worse swap than the program calculated. It does NOT establish a fair price: manipulation before the instruction can still worsen that quote. Slot-opening snapshots are an optional earlier guard, not a trusted oracle or proof that #287652 is solved.

### What happens when every price bound fails

1. Before terminal permission, protected/guarded fills revert if their bound fails. Reverts cannot leave a failed-attempt marker. The public distress observer can still succeed independently.
2. At terminal permission, the program may waive historical execution-price bounds only for its own full internal sale. The old EMA value cannot permanently veto the sale.
3. Net recovery after the bounded keeper reward repays principal first. Eligible insurance covers remaining principal subject to event/day limits. Remaining principal is written off; unpaid interest is canceled, not booked as fee income. The debt leg ends at zero.
4. If the AMM physically cannot execute (unavailable output cash, invalid curve, zero-credit token transfer), price-floor relaxation does not create liquidity. Distinguish this condition from a low but executable price. Do not clear debt while abandoning valuable unsold collateral. The existing zero-credit cleanup rules need a separate, explicit accounting treatment. The current simulator rejects cash-inconsistent scenarios and reports unprofitable cleanup as unresolved; zero-credit transfer behavior still needs native integration.
5. A permissionless keeper must submit the transaction. Percentage incentives cannot guarantee cleanup when proceeds are zero or the reward is below execution costs. Report this boundary; an upfront deposit, subsidy, batching or minimum size remains a separate product decision.

**Accepted modeling tradeoff:** terminal disposal can realize price manipulation losses. Report those losses alongside crash losses; do not rename them as slippage protection. If the measured exposure is unacceptable, change admission/margins or the terminal policy explicitly. Atomicity cannot distinguish genuine market decline from manipulation of the same pool.

## 2. Distress observation and reset rules

Track an explicit distress episode independently of the in-transaction flash lock. Normal owner actions never wait for the distress timer.

| Event | Clock behavior |
| --- | --- |
| First fresh observation with reference health <= MM | Start the episode; confirmation age is zero. Preserve existing executable eligibility for actual liquidation. |
| Another observation in the same slot | Adds zero age. Transaction/instruction counts are not elapsed time. |
| Later qualifying observation within the freshness window | Add elapsed slots once. Require at least two distinct observations for the age-based fallback. |
| Observation gap exceeds the configured maximum | Start confirmation again from the fresh observation; do not backdate distress across unobserved hours. Critical current reference health can still authorize the independent terminal condition. |
| Owner adds margin or repays but reference health is still <= MM | Preserve the episode. Amount transferred or instruction name is not a reset condition. |
| Reference health is actually > MM | Clear confirmation. A later breach begins a new episode. The model uses this simple rule; hysteresis is not silently added. |
| Successful partial restores health | Clear only after recomputing post-settlement health; subsequent real distress starts a new episode. |
| Failed begin/route/settle | No persistent changes from that transaction. A previous successful observer state survives. |
| Full close or fully repaid debt leg | Clear the episode and corresponding active session. |

Observe using a current canonical reference before any risk-increasing mutation, and after owner risk-reducing mutations. Permissionless observation must not lock collateral, require the owner signature, pay a repeatable bounty, or block repayment/owner close. A successful transaction may mark distress without beginning a liquidation. If begin would revert for insufficient age, the observer must be separable so its update can persist.

These observations establish sampled distress under the accepted competitive-keeper assumption, not proof that health never recovered between observations. The reference itself is endogenous and may be influenced across slots. A long period without observers cannot be represented as confirmed continuous distress.

## 3. Incentives, insurance and accounting

Use the same bounded progressive reward formula in partial, protected full and emergency modes. No separate reward jump merely for choosing emergency. Compute escalation from reference health and confirmed age, never instantaneous executable health. Partial sizing includes reward, AMM/transfer fees and any insurance contribution; a partial that fails its health target must revert.

For the comparison, the reward is 0.5% plus up to 2.5 percentage points as reference health deteriorates, capped at 3%; a 0–0.5 percentage-point age supplement is included inside that cap. It is paid from actual route proceeds, including insolvent cleanup. These values are sensitivity inputs, not a recommendation.

The protected route floor discounts reference collateral value by a bounded health/age-dependent amount. This is economically a descending minimum sale price, even though settlement is a single atomic flash transaction. Count the discount AND explicit reward in owner cost. A keeper can retain route value above its obligation; report both observed-vault-surplus and floor-only delivery cases rather than assuming competition refunds all outside profit.

Suggested shortfall accounting for calibration:

```text
route credit - bounded reward = available sale recovery
sale recovery -> principal -> interest actually funded by sale -> owner residual
insurance -> remaining principal only, within existing caps
remaining principal -> write-off
remaining unpaid interest -> cancel, no protocol/referral fee accrual
solvent residual -> bounded insurance contribution -> owner
```

Principal-only insurance and a liquidation contribution on leverage are proposed economic refinements, not previously approved runtime defaults. Compare them before implementing. Borrowing currently tracks aggregate principal: assigning principal to a liquidated borrow leg must follow an explicit allocation rule; do not substitute an invented per-position principal field. A solvent partial must also budget any selected insurance funding inside its total penalty allowance.

## 4. Margin alternatives to compare

All candidates retain an aggregate-exposure IM requirement and entry checks against both reference and executable value. Compare `IM >= MM + buffer` with `IM >= 2 * MM`; the latter is a candidate relationship, not an adopted Hyperliquid policy.

| Candidate | Maintenance basis | Status |
| --- | --- | --- |
| Fixed amounts | Historical 10k/30k debt-token-unit bands, 7/8/10% marginal rates | Control showing scale/denomination sensitivity; not universal USD bands. |
| Depth-relative gentle | Position collateral / snapshotted collateral-side depth; 20%/60% depth bands, 7/8/10% | Same gentle schedule normalized to depth; tests whether normalization alone suffices. |
| Depth-relative earlier | 10%/30% depth bands, 10/20/35% marginal rates | Stress comparator; explicitly higher MM and lower leverage, not a selected default. |
| Stack-at-entry earlier | Marginal maintenance amount assigned across aggregate same-direction exposure using the earlier schedule | Product alternative requiring approval; entrants at different times get different terms. |

For a depth snapshot, use collateral-token units for BOTH numerator and denominator. Revalue both consistently if expressing them in debt tokens. Risk increases must not cheaply refresh the whole old position onto softer terms. Stack assignment must integrate marginal amounts across the interval, not apply the endpoint rate to the whole new position. A fixed-denominator telescoping identity proves splitting neutrality only for that controlled case; entries moving liquidity, withdrawals, increases and partial closures require separate validation.

Tail-only conservative depth is a candidate and must remain cash-bounded. The comparison's CPMM fixtures cannot settle whether concentrated-band depth should be recognized. Do not claim that a borrowed receivable, hLP synthetic balance or transient LP deposit provides independently available liquidation cash.

## 5. Calibration contract and UX requirements

The economic simulator is separate from production Rust. Compare executed sequences of partial/full settlement, progressive rewards, event/day insurance budgets, write-offs and their pool-price effect. Measure admission, unresolved debt, total principal loss, insurance use, keeper proceeds, owner residual and time to completion. Keep initial position sets/admission differences visible when comparing loss numbers.

Use native zero-fee CPMM liquidation fixtures in both debt directions and asymmetric prices/decimals to anchor the independent model's quote, reserve and socialization arithmetic. This does not certify the new policy, transfer-fee accounting, concentrated curves, public borrowing or transaction locks; those require native/LiteSVM integration before merge.

User-facing requirements:

- Preview resulting IM/MM and an estimated liquidation price, plus all costs. Bind acceptable economic terms so an opening transaction cannot silently assign worse maintenance than the user accepted.
- No extra trader approval or separate trader transaction to mark distress. Keep observer and route complexity in keeper/SDK flows.
- Owner repayment/add-margin/full close remain available under normal guards. Crowding IM applies to new risk, not a rescue payment.
- No upfront cleanup deposit, new minimum position size or new LP withdrawal restriction is selected here.
- Maintain partial positions in events/indexers/UI and display actual remaining debt/collateral. A distress timer is not the flash session lock.

## 6. Review and PR coordination

PR #45 remains at `5b123acf`; main remains `e8f12ea7` as verified 2026-10-02. PR #48 (`feat: add native collateral leverage support`, head `65a948f`) is now also open and overlaps leverage state/transitions, open/close handlers, SDK interfaces and the main leverage test module. This work adds documentation and test-only calibration; integration of runtime changes must reconcile #48's new entry/close paths with session guards, exposure accounting and margin updates. Do not claim #45 is the only open PR or change #48 from this task.

Finding #287652 remains open as an execution-price risk. No historical exploit PoC is executed by this work.
