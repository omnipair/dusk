# Dusk elected market manager implementation plan

Prepared: 3 November 2026. Status: design and implementation handoff. Protocol implementation has not started.

Add an optional elected manager to each market while retaining LP governance, including its existing direct, delegated, senior, and junior participation paths. Managers propose permitted market changes without gathering LP support. Every manager change retains at least the existing seven-day timelock; the existing ten-day notice for bundles containing tranche changes must not be shortened. Reuse the atomic multi-family proposal mechanism already on main. LP governance can cancel manager proposals and revoke the manager.

The purpose is to let LPs remain passive during routine management while retaining control over appointments, policy, and harmful changes. Token issuance, staking, and market revenue sharing are separate extensions; they do not determine who may change market parameters.

## 1. Agreed direction and decisions to settle

### Agreed direction

| Topic | Direction |
| --- | --- |
| Authority | An optional manager is elected through LP governance. Existing LP parameter proposals remain available. |
| Manager proposals | No LP sponsorship, support threshold, or LP burn-lock is required to submit a manager proposal. |
| Timelock | Manager parameter changes retain the seven-day wall-clock minimum. Preserve longer existing family notice requirements, including ten days for tranche changes. Announcement and execution are separate transactions. |
| Atomic changes | One proposal may contain related typed parameter changes. Every component executes together or none executes. |
| Cancellation and removal | Successful LP-majority cancellation or manager removal takes effect immediately, without an additional timelock. Parameter changes retain their full notice periods. |
| Existing endorsements | Keep support for unchanged absolute targets and stated conditions when starting settings move during collection. Revalidate at majority, seal execution assumptions, and start the full timelock. Changed targets or invalidated endorsed conditions require fresh support. |
| Retired junior cleanup | Allow owners to clear obsolete governance receipts through the normal withdrawal flow after proposal locks end. No wiped principal is restored; historical earned yield remains claimable. |

There is no global first-in-first-out queue. A proposal is a stored commitment with an execution time. An unrelated pending, cancelled, stale, or expired proposal must not block another proposal merely by existing.

### Recommended defaults requiring design confirmation

| Decision | Starting recommendation | What must be resolved |
| --- | --- | --- |
| Appointment and mandate changes | Elect, replace, or expand a mandate through normal LP governance and its timelock. A market may have no manager. | Optional term expiry, resignation, manager self-cancellation, and whether narrowing a mandate follows the ordinary delay or a distinct removal path. |
| Manager permissions | An explicit allowlist with market-specific ranges and cumulative movement limits inside protocol hard bounds. | Allowed fields and calibrated numeric limits. Existing protocol maxima are not automatically manager allowances. |
| Proposal conflicts | Use revision checks for every proposal and temporary dependency reservations for majority-backed LP proposals. Managers receive no conflict-check exemption. | Confirm the dependency matrix, competing LP reservation rules, and how explicitly endorsed conditions are encoded and checked. Keeping unchanged target endorsements is agreed; exact dependency reservations still need implementation detail. |
| Future extension scope | Keep tokens and reward programs external. Reuse existing governance delegation and document the missing native per-market revenue-allocation capability. | If future native per-market fee sharing must work without a Dusk upgrade, its bounded allocation interface needs a separate design now. Do not claim it already exists or silently add fee policy to this implementation. |

This design-plan PR does not select unresolved policies or authorize protocol implementation, deployment, token issuance, or a fee allocation.

## 2. Current implementation and scope

Baseline: main at `bd980852fa743be609896109307f4084732eddf7`, refreshed on 3 November 2026. The current implementation already includes atomic bundles and reusable delegated governance balances. Recheck the implementation checkout before starting and preserve unrelated work. Dusk is pre-deployment; do not invent production migration requirements or use a production launch gate as a substitute for resolving a design risk.

| Area | Current behavior and implementation surface |
| --- | --- |
| Proposal model | [parameter_proposal.rs](../programs/dusk/src/state/parameter_proposal.rs): one to nine typed updates in canonical family order, at most one per family, V2 digest binding updated-family revisions, and atomic execution. |
| LP authorization | [proposal.rs](../programs/dusk/src/instructions/governance/proposal.rs), [support.rs](../programs/dusk/src/instructions/governance/support.rs), and [governance helpers](../programs/dusk/src/instructions/governance/mod.rs): 1% sponsorship, strict majority to queue, seven-day ordinary or ten-day tranche timelock, seven-day execution window, and direct plus reusable delegated support. Eligible voting value combines direct senior shares with active junior receipt value; hLP-held shares and permanent/queued junior receipts are excluded under the current rules. |
| Execution and safety | [governance transitions](../programs/dusk/src/transitions/governance.rs): validate a combined target, checkpoint old state, execute all selected families, roll back failures, and advance changed-family revisions. Oracle-only updates have an existing special admission path. The stale-proposal handler can return success without applying an update. |
| Market storage and delegation | [market.rs](../programs/dusk/src/state/market.rs) has nine parameter revisions and no manager mandate. [GovernanceBalance](../programs/dusk/src/state/governance_balance.rs) and [GovernanceSupport](../programs/dusk/src/state/governance_support.rs) already provide reusable owner balances, committed allocations, and revocable delegates. Extend these deliberately rather than inventing a second ownership system. |
| SDK and tests | [governance.ts](../packages/dusk-sdk/src/governance.ts), [write.ts](../packages/dusk-sdk/src/write.ts), [SDK governance checks](../packages/dusk-sdk/scripts/check-governance.js), and [LiteSVM suite](../tests/v2-final-smoke.test.ts). |

The planned change covers manager election and removal, mandates, atomic parameter bundles, cancellation, conflict handling, interfaces, and adversarial validation. Preserve LP parameter governance and existing emergency/protocol authority separation.

Revenue tokens, emissions, staking economics, and automatic per-market protocol-fee entitlements are separate work. Existing governance delegation already lets an owner appoint an active participant without handing over withdrawal or yield authority; it still requires governance deposits, support thresholds, and timelocks. A market manager is a distinct permission to submit without collecting that support. Existing global protocol-fee recipients, referral interest sharing, and owner-selected LP yield recipients do not constitute an arbitrary native market-token fee entitlement.

## 3. Authority and proposal model

### Market mandate

Specify a market-scoped mandate containing the manager authority, a monotonically increasing manager epoch, the approved policy and its revision, and any selected expiry. A manager can be a wallet or a program-derived signer controlled by another program. Each market grants and revokes its own mandate, even when one authority manages several markets.

Only LP governance can appoint a manager or expand/change the mandate under the chosen policy. Managers cannot grant themselves new powers, replace themselves, change voting rules, or shorten the timelock. Manager authority grants no reserve custody, LP mint authority, user withdrawal authority, protocol revenue administration, emergency authority, or program upgrade authority.

On replacement, removal, or expiry, manager proposals from the old epoch become ineligible for execution. Appointment, replacement, and mandate-expansion proposals must also bind the authority epoch and policy they intend to replace, so an earlier pending action cannot silently reinstate a removed mandate. A policy revision must not silently widen a pending proposal's authority; use explicit invalidation and a fresh announcement when authority changes. Removing a manager does not reverse already executed changes or losses.

Prefer a separate market-scoped governance control account if it avoids inflating the hot market account, but measure its account-count, transaction-size, heap, and compute costs before choosing the layout. A PDA signer that must also fund proposal creation needs a valid funded payer path; verify this in a CPI fixture rather than assuming every PDA custody arrangement works.

### Typed actions and bundles

Reuse the existing explicit parameter-update bundle for both LP-backed and manager-originated parameter proposals. Represent appointment/mandate changes, manager removal, and cancellation as distinct governance actions with their own authorization and timing rules. Do not permit arbitrary instruction execution by the market PDA.

For a parameter bundle:

1. Include a nonempty bounded collection of typed updates, with at most one update per parameter family and canonical family ordering.
2. Extend the existing digest to bind origin and the applicable mandate epoch/policy revision. Bind complete target values, metadata, changed-family revisions, and program-computed configuration dependencies. Use distinct linked support-intent and execution-snapshot commitments rather than mutating a digest that LPs already endorsed. Version changes and keep Rust/SDK serialization identical.
3. Commit the complete payload before its full notice period starts. Editing any component requires a new commitment and timelock. Governance cancels the bundle as a unit.
4. Require execution in one transaction under a measured action/byte/compute bound. No public path may execute only a component. If a desired bundle cannot fit, do not silently split it into transactions and call it atomic.
5. Preserve the current seven-day execution window as the initial design baseline. Expiry and permissionless cleanup must not depend on the manager remaining available.

Proposal origin is immutable and controls the lifecycle: manager proposals intentionally have zero LP sponsorship, so LP sponsorship-floor cancellation and last-supporter cleanup must not classify a pending manager proposal as cancelled or removable. Keep a stable minimal identity and terminal status for cancellation references; releasing the final LP allocation must not erase an object another active ballot still needs. Normal LP proposals retain their sponsorship/support process before their timelock starts. A valid manager proposal starts its timelock when submitted. A bundle is all-or-nothing: a fees-plus-concentration change cannot execute only its fee component, and a failed component rolls the whole operation back. Manager proposals do not require the manager to custody LP capital.

### Recommended conflict policy

Every proposal, including a manager proposal whose timelock has elapsed, must satisfy revision checks. There is no manager exemption. Reuse current write-family revisions and add configuration dependencies computed by the program from the typed actions; callers cannot omit a dependency. If a bundle depends on concentration while changing only controller settings, it must also check concentration's revision. Track configuration changes, not ordinary swap-driven curve/risk observations, so trading does not continually stale proposals. Continue live market safety checks separately.

For example, two manager proposals announced against fee revision 12 cannot both independently overwrite that state: the first successful fee update advances to 13, and the other becomes stale as a whole. Unrelated valid proposals may execute in either order. For a multi-family bundle, a mismatch in any required revision invalidates the whole bundle.

Once an LP-backed proposal reaches its required majority and enters its timelock, reserve the configuration dependencies it needs against conflicting manager writes. A manager proposal announced earlier or already executable receives no exception. Reservations affect parameter changes, not trading, repayment, or LP ownership. Opening a proposal or reaching the 1% sponsorship floor reserves nothing.

Reservations are market-scoped and dependency-scoped, never a global FIFO. A reservation is effective only while its owning proposal remains eligible. Release or ignore it on execution, cancellation, expiry, stale dependency revisions, and invalidated authority/policy. Derive that from current state and the clock without depending on a cleanup keeper. Pending-capacity accounting must use the same eligibility predicate so invalidated old-epoch proposals do not exhaust a replacement manager's slots. A conflicting manager proposal may execute afterward only if its original execution window, revisions, mandate, and policy are still valid; blocking does not extend its deadline. Specify deterministic rules for two majority-backed LP proposals requesting overlapping reservations, and acquire a bundle's reservations all-or-nothing. A cancelled or expired proposal must not retain a lock.

The agreed collection policy binds LP support to immutable absolute targets and disclosed conditions, then validates the live starting state and seals execution revisions at majority, when the full notice period begins. This avoids a manager continuously invalidating collecting LP proposals merely by changing the starting revision. The existing V2 format instead binds revisions at creation, so this policy requires a deliberate intent-digest/execution-commitment extension. An endorsement conditional on an earlier configuration cannot be silently rebased; preserve that condition or obtain fresh support. A changed target or invalidated endorsed condition requires a new intent and fresh support; an unchanged target with valid conditions keeps its existing endorsements.

## 4. Bounds and atomic execution

### Review all nine current parameter families

| Family | Required manager-policy analysis |
| --- | --- |
| Fee profile | Separate routine fee changes from compounding, fee denomination, launch schedules, rate limiting, volatility signals, and fee sensitivity. These fields are bundled together today but need not receive identical manager permissions. Test trading suppression, related-party trading, and indirect exit/settlement effects. |
| Concentration | Bound shape changes, executable price/depth effects, and protected-liquidity funding. Test both hLP sides and borrowers. Do not assume a time ramp from older prose: inspect the current shape-update implementation. |
| IRM | Bound immediate rate effects and future adaptation. Preserve accrued interest and the adaptive anchor. A rate increase or decrease is not automatically harmless. |
| EMA half-lives | Bound sensitivity and frequency while preserving accumulated history. Test manipulation persistence, liquidation/admission signals, and controller behavior. |
| Daily borrow limit | Preserve gross borrowing history and refill accounting. Assess cash consumption and outstanding exposure; a bounded daily parameter alone does not guarantee withdrawal liquidity. |
| Center controller | Bound movement speed, thresholds, intervals, and cumulative changes. Preserve funded recenter requirements and deferred-target accounting. |
| Insurance draw caps | Review principal coverage and daily draw policy against the current implementation. Analyze loss allocation to insurance and LPs. Lowering insurance access can increase LP losses; tightening a numeric cap is not a general safety proof. |
| Oracle | Review authenticated observations, feed/reference binding, blend policy, birth-config bounds, and transitions into or out of native pricing. Preserve the oracle-only recovery path and reject privilege expansion through mixed bundles. |
| Tranche | Review junior income share, request/liquidation fees, loss-bearing incentives, and senior/junior voting value. Preserve the existing ten-day notice and protocol-defined junior exit timing. Manager permission over these fields remains an explicit mandate decision. |

For each allowed field specify protocol bounds, mandate bounds, per-change limits, time-window/cumulative movement budgets, and relevant cross-field constraints. Reject changes to non-allowed fields even when another field in the same profile is allowed.

Specify when a movement budget is reserved and consumed, how overlapping pending bundles are counted, and how cancellation/expiry releases a reservation. Announcing many individually permitted changes must not create a stockpile that bypasses cumulative limits when executed together. Revalidate live policy and budgets at execution; cancellation must not refund movement already executed. Include absolute bounds and path variation where needed to prevent repeated small or alternating changes from evading a limit. Keep executed rolling usage market-scoped across manager changes. Releasing a cancelled or invalidated unused reservation is idempotent and never erases executed usage; a new mandate does not reset the market's movement history.

### Shared execution transition

Checkpoint elapsed interest, fee/yield state, EMAs, risk, and relevant rolling limits under the old configuration once. Construct and validate the combined target, then apply the complete transition through shared checked primitives. Preserve intermediate accounting invariants and validate final market safety without retroactively evaluating elapsed state under new parameters.

A successful execution updates every affected revision and emits the complete outcome. Any failed component leaves configuration, balances, debt, LP/hLP claims, controller/protected-liquidity state, revisions, and executable proposal status unchanged.

Distinguish a terminal stale/cancelled result from successful execution. The existing success-returning stale path must not let an executor or SDK report that all bundle components applied when they did not. Decide whether explicit terminal status plus events/return data or a separate status transition provides that distinction.

Keep the existing economic execution checks and oracle-only recovery exception for parameter changes unless a demonstrated safety argument justifies changing them. Administrative cancellation/removal must remain independent of the 80% utilization guard, AMM solvency, trading activity, and fragile price/controller checkpoints.

Permissionless withdrawals are not guaranteed instant redemption: current yLP withdrawals need cash on both sides and hLP exits have settlement conditions. A seven-day notice period does not supply missing liquidity. Model loss, withdrawal obstruction, repayment obstruction, and recovery explicitly; do not promise unrestricted exits without a separate liquidity design.

## 5. Governance cancellation and griefing controls

### Ballot and ownership accounting

Opening an objection or cancellation ballot grants no veto, freezes no parameter family, and changes no execution timestamp. A successful authorized vote cancels the exact target proposal. Maintain one canonical cancellation ballot per target so duplicate objections cannot create competing state or repeated pauses.

Use the existing strict LP-majority rule for final cancellation/removal, with immediate effect once reached. Preserve the current senior/junior voting basis unless a separate product decision changes it. Define junior live exchange-rate and generation behavior, excluded permanent/queued receipts, denominator timing, mint/burn/deposit/withdrawal behavior, and majority latching before implementing handlers. A historical snapshot cannot be inferred from a current token balance; either provide authentic history or specify a locked-ownership mechanism with explicit live-denominator semantics.

LPs with senior or junior ownership already locked in direct or delegated governance must remain able to participate in cancellation/removal. Extend the existing GovernanceBalance/GovernanceSupport accounting, with verified references to direct ProposalSupport where necessary, a per-ballot receipt, and an explicit encumbrance lifecycle. Existing support accounts can close on remint; references must survive or be settled before closure. Preserve their virtual yield and exact principal claim. Bind junior vote aggregates, contribution admission, and global active lock inventory to the matching cohort mint/generation. Retiring one generation must not revalue its votes or debit its later cleanup against a replacement generation. Historical yield must continue to use the retired cohort's final indexes and exposure end. Allow owner-authorized cleanup through the existing withdrawal flow after allocation locks end. Clear the obsolete governance amount without consuming replacement-generation inventory or erasing historical yield. Test the dependency implementation across rollover before reusing it.

Prove that the same economic ownership cannot be counted twice within a ballot through transfers, account splitting, support withdrawal/remint, or multiple references. If an existing lock backs a cancellation vote, its release must not permit that vote to remain counted while the reminted yLP votes again. Participating locks need a finite release condition on cancellation, execution, expiry, or manager-epoch invalidation. Opening an unsuccessful ballot must not encumber nonparticipants.

Reuse actual governance delegation; ordinary SPL token approval and the separate harvest authority are not substitutes for it. Do not count a momentary unlocked balance as durable veto weight. Capital acquisition, borrowed voting power, and concentration remain economic capture risks to evaluate, even when transaction-level double counting is prevented.

### Cancellation and execution boundary

Latch cancellation/removal in the transaction that first satisfies the required LP majority and make execution inspect the authoritative state directly; no extra timelock applies. There must be no additional finalize transaction whose omission lets the manager execute a defeated proposal. Define whether voting remains open throughout the execution window and the exact timestamp comparisons. A vote included after execution cannot undo that execution.

Bind cancellation to the proposal identity/digest. Cancelled proposals cannot be revived, edited, or executed. A manager replacement/resubmission needs a fresh notice period. Successful manager removal invalidates all outstanding proposals for that mandate epoch without scanning an unbounded proposal list.

### Required adversarial cases

| Attack or failure | Required behavior |
| --- | --- |
| Minority opens or duplicates objections repeatedly | No pause, timelock extension, or global reservation before an authorized outcome. |
| LPs already locked in another proposal cannot reach a removal majority | Their economic ownership remains usable under the verified voting design without creating duplicate claims. |
| Manager repeatedly changes a family to stale LP governance | Manager-only activity cannot indefinitely defeat an LP-backed override or authority ballot through revision churn. |
| Tiny LP proposal freezes management indefinitely | Merely collecting sponsorship creates no indefinite manager reservation. Define finite priority once the agreed governance condition is met. |
| Manager floods, overlaps, or pre-announces many bundles | Bounded pending state and cumulative exposure; no dependency on iterating a global list. Unrelated proposals remain executable. |
| Veto is hidden by expiry, cleanup, or transaction ordering | Status, release rules, boundary comparisons, and execution checks are deterministic. A threshold already reached cannot be bypassed by omitting finalization. |
| Market stress blocks administrative recovery | Cancellation, manager removal, and their terminal lock release have no unnecessary AMM health/utilization dependency. |

A minority binding veto necessarily gives that minority obstruction power. If a lower threshold is desired, present that tradeoff explicitly rather than claiming a spam deposit or cooldown removes it. Restricting only nuisance behavior must not become a reason to remove a useful market feature or introduce production-only gates.

## 6. Implementation sequence and completion checks

1. **Finish the authority and voting specification.** Finalize the remaining mandate, dependency, bound, and extension details in section 1 while preserving the agreed support, cancellation, and cleanup policies. Produce the action/bound matrix, state transitions, denominator/lock proof, conflict rules, and economic attack cases. Completion: no ambiguous source of voting weight, pending-proposal priority, or cancellation timing remains.
2. **Extend the existing bundles and shared transitions.** Add manager origin, dependency revisions, the selected support-intent policy, reservations, and lifecycle outcomes while preserving existing canonical serialization, combined validation, checkpointing, and rollback. Keep LP-backed governance operational. Completion: multi-family success and failure tests prove all-or-nothing behavior, including invalid final actions and stale conflicts.
3. **Implement mandates and administrative recovery.** Add election/mandate actions, epoch-bound manager submissions, cancellation/removal ballots, expiry/cleanup, and the reviewed manager bounds. Completion: the seven-day minimum and longer existing family notices are unavoidable; governance cannot be evaded by revision churn; stressed markets and old epochs cannot strand administrative recovery or governance claims.
4. **Integrate SDK, CPI clients, events, and documentation.** Update digest helpers, types, PDA derivation, builders/readers, status handling, instruction coverage, and examples. Add an external-program fixture that owns yLP and exercises proposal/support/remint plus manager submission where elected. Measure the largest admitted bundle with maximum supported metadata. Completion: external callers work within the actual account, transaction, heap, stack, and compute budgets.
5. **Run adversarial validation and the complete required checks.** Exercise the matrix below and regenerate all required interfaces. Completion: all required checks pass, every new instruction is covered, and no unresolved economic risk is hidden by disabling scope or changing release policy.

### Validation matrix

| Area | Minimum meaningful evidence |
| --- | --- |
| Atomicity and accounting | Fail each bundle component and verify full rollback. Check old-parameter accrual, both hLP sides, debt, fees, protected inventory, deferred controller state, and all affected revisions. Cover independent-update ordering and explicitly specified dependent-update ordering. |
| Authorization and timing | Wallet and PDA managers; wrong market/authority/epoch/policy; election/replacement; unauthorized fields; before/at/after ready time and execution deadline; changed payloads; partial execution attempts. |
| Voting and liveness | Majority boundaries; locked and liquid yLP; changing supply and hLP exclusions; transfer/split/remint reuse; support-account closure; finite claim release; unsuccessful objections; no-manager operation; stressed-market removal. |
| Economic manipulation | Repeated fee/shape/EMA/IRM changes, launch-field changes, related-party trades, borrowing before LP exits, hLP settlement disruption, insurance-loss shifts, budget stockpiling, and cancelled-bundle replay. Distinguish reproduced harm from nuisance and unproven hypotheses. |
| Runtime and composability | Maximum bundle and metadata size, full transaction serialization, CU baselines, stack/heap bounds, permissionless execution, event recovery, SDK digest parity, and the external-program CPI fixture. |

Use the existing Rust governance/state tests, extend the SDK governance checks, and add instruction coverage to the LiteSVM suite. Resolve the currently unsupported interface capabilities through explicit adapters or scoped core work; do not turn an untested static assumption into a no-upgrade guarantee.

### Required interface regeneration

Treat instruction accounts, arguments, return types, events, program IDs, and public Rust documentation as interface changes. Before the first Anchor command in a clean checkout, run `node scripts/prepare_build.js` to preserve program IDs. Install dependencies with the current postinstall Anchor type patch, or run `node scripts/patch_anchor_idl_types.js` once for older dependency caches. After Dusk or leverage-delegate interface changes, run:

```sh
anchor build -p dusk
anchor build -p leverage_delegate
npm run prepare-idl --prefix packages/dusk-sdk
npm run check:dusk-sdk
```

Include the regenerated `packages/dusk-sdk/src/idl_v2.json`, `types_v2.ts`, `idl_delegate.json`, and `types_delegate.ts` with the corresponding source change. After a faucet interface change, also run `anchor build -p faucet`; its IDL is not currently vendored.

### Required checks before any implementation commit

Follow [AGENTS.md](../AGENTS.md) and [.github/workflows/ci.yaml](../.github/workflows/ci.yaml). Run the full sequence, and do not commit with any required check failing:

```sh
cargo fmt --all -- --check
yarn check:hygiene
yarn check:code-shape
yarn check:clippy
cargo test -p dusk
cargo check -p dusk --lib --features production
cargo test -p dusk --lib --features production
cargo test -p dusk --lib --features benchmark ema_admitted_settlement_preserves_reference_and_resource_delta
cargo test -p leverage_delegate
cargo test -p faucet
yarn typecheck
yarn build:litesvm
yarn check:build-identity
git diff --exit-code -- Anchor.toml programs
npm run check:dusk-sdk
DUSK_REQUIRE_COMPLETE_CU_BASELINE=1 yarn test-litesvm:no-build --forbid-pending
```

Also run `npm run test:governance --prefix packages/dusk-sdk` for the changed governance helpers/builders. Before any branch creation, renaming, push, or PR action, follow the user's branch-prefix memory and current repository instructions. This PR publishes the design plan only; protocol implementation remains separate.

The first implementation deliverable is the reviewed manager action/bound matrix and voting-accounting specification, followed by the atomic bundle transition.
