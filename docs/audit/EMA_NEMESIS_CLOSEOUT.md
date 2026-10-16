# Nemesis closeout — verified result

**Disposition update, 2026-10-17:** the user authorized closure of #287652 with
accepted residual inventory/arbitrage risk after remediation of the original
live-price/depth dependency. See the [acceptance record](../calibration/EMA_SETTLEMENT_CLOSEOUT.md).
The dated review below is preserved as evidence of what was known before that
acceptance; its references to an open disposition are historical. No new audit
or runtime change is claimed by this update.

Runtime 5a9abcb3; 2026-10-16. Full Feynman -> full State (12 coupled relations) -> targeted Feynman, converged after three passes. Scope and evidence are in [the closeout summary](../AUDIT_CLOSEOUT.md) and the dependency map below. This is not a fresh exhaustive audit of every change in #45.

No new verified runtime accounting/security defect in this scoped review. #287652 original direct live-price/depth dependency is mitigated; residual inventory-related trading extraction remains open. #287160 has implemented mitigation with retained issuer and keeper-execution risks. Neither a zero-loss claim nor a universal-liveness claim is made.

New native regression: 24 admitted states, 144 rows, 48 preceding-trade comparisons; exact quote equality and exact per-asset accounted-resource delta equality, debt clearance and native invariants. Existing default/SVM accrued-funding/credit tests and complete mandatory CI are summarized in the closeout validation section. Native economic comparison is not an instruction-level exploit proof. Historical supplied PoC not run.

Raw hypotheses H1/H2/H3/H5 rejected with called-path/regression evidence; H4 retained as economic risk and clarified through pass2->pass3. No speculative new severity is assigned. No program behavior, interface, deployment or product policy changes in closeout.


## Pass 2 — State inconsistency (full scoped pipeline)

Enriched by H1–H5 from the first Feynman pass. Pairs are actual jointly read state, not assumed equalities.

| Coupled relation | Mutation paths / ordering | Result |
| --- | --- | --- |
| Virtual reference vs real reserve ledger | Constructor only mutates clone; inventory flag forces SettleLiquidation and forbids prepared virtual cache | Designed distinction maintained |
| Prepared quote vs revision / one-use transition | prepare captures revision; bind validates policy/cash; apply checks revision, consumes transition/tag/cache | No reuse or stale actual plan |
| Aggregate debt vs position shares/principal | lifecycle offsets fixed/isolated debt; apply_liquidation_position removes matching shares/collateral; allocation revalidated | Partial/full checked, one-atom rounding explicitly reconciled |
| Principal loss vs live/curve reserve | lifecycle cancels unpaid interest separately; apply_leverage_socialized_loss debits principal and rebases actual curve | No unpaid interest insurance/fee creation |
| Fees vs ownership indices | Allocation/checkpoint uses pre-reconstruction hLP shares; then fresh ownership reconstruction | No reassignment to new shares |
| hLP debt/ownership vs real curve/cash | prepare_at_current_state after inventory/loss; consume validates start, funding floors and materialized endpoint | Existing funding and native invariant tests pass |
| Insurance draw vs reserve credit/budget | observed CPI credit, consume_draw, full-only capacity and principal caps; fee topup separate | No second draw from quote/application |
| Physical reserve/custody vs payouts/protection/backing | measured token credits, exact receiver deltas, final custody guards; independent native resource sums | Exact per-asset settlement delta unchanged across 48 preceding-trade comparisons |
| Curve geometry vs protection metric | actual cache reconstruction; loss checkpoint consumes positive budget, cannot mint spendable gain | Intended loss asymmetry, not missing update |
| Curve revision/actual price vs risk/volatility | finalization uses actual start/end; observes actual final curve depth | No virtual price observation |
| Position debt/collateral vs global health/distress | partial updates contribution and eligibility; full clears observation | No old auction state retained |
| Public quote vs SDK/event fields | Same quote payload and IDL, caller minima default to preview | No execution/preview schema mismatch |

Parallel paths checked: ordinary swap vs internal emergency; internal vs external fixed payment; partial vs full; borrowing vs leverage; zero-credit cleanup vs nonzero inventory settlement. Ordinary external routes intentionally do not commit EMA virtual inventory.

Multi-step checks: native admission -> exogenous swap -> EMA -> preceding swap -> emergency -> subsequent swap; LP withdrawal with fixed EMA; repeated prepared apply; accrued funding -> emergency -> canceled interest; partial repayment/recovery versus full bound. Existing SVM credit/rollback tests are rerun in full CI.

Masking reviewed: min(principal,recovery), saturating unpaid-interest remainder and insurance caps express loss allocation; unknown singleton-proof errors refuse full permission. No silent successful settlement of a failed proof.

Delta: H4 resolves to economic inventory/trading risk rather than demonstrated state desynchronization. At the common mark the liquidation resource delta is identical in every comparison. In the example LP/hLP claims fall 1,619.453, but 1,460.871 remains in protection; total accounted token resources fall 156.182 after subsequent trading. This is still economically relevant and is not dismissed as harmless accounting.
New dependency for pass 3: controller protection deployment can move value between executable inventory and protected buckets without changing accounted token resources. Confirm its authorization/invariant before declaring root cause.


## Pass 3 — targeted Feynman re-interrogation

Delta from pass 2: controller protection deployment / inventory trading.

Reviewed try_apply_deferred_controller_target and actual loss cache refresh. Protected tokens become executable only when the candidate curve-depth/share funding check succeeds; pending/reachable/slot checks remain. The physical bucket is zeroed when credited to live/cash, so it is not counted twice. Reference settlement never substitutes its virtual cache. Current-state hLP reconstruction validates the real source endpoint; fee growth is checkpointed before ownership changes.

The original report concerned live AMM execution determining repayment/loss. Current quote and token-resource deltas are invariant in the admitted same-reference comparisons. Later trades differ because real starting inventory plus fixed liquidation inventory changes the real curve; controller/protection adds path dependence. An accounting-equivalence correction has not been identified. Changing trading eligibility, reserve deployment or inventory policy is a product/economic change and is not silently made.

Economic sequence reproduced natively with costs and no-liquidation controls; exact on-chain bundle feasibility remains outside this review. No new coupled pair, root cause or code defect emerges. Converged after 3 alternating passes. #287652 residual economic disposition stays open.

## First-pass scope and rejected hypotheses

The initial Feynman pass reviewed all changed EMA curve construction, preparation,
application, fee reference selection, partial/full sizing, actual reserve
checkpoint and SDK reference-output functions. Shared emergency instruction,
debt lifecycle, position/global-health updates and hLP reconstruction were traced
as dependencies. Purpose, order, sibling behavior, assumptions, boundaries,
errors and multi-operation composition were checked within that scope.

- H1: virtual reference overwrites real balances — rejected: scratch state is
  discarded; the inventory tag forbids the virtual prepared cache.
- H2: an old hLP plan restores written-off value — rejected: actual loss is applied
  before fresh endpoint construction; fee ownership is checkpointed separately.
- H3: the fixed invoice changes from the same-slot preceding trade — rejected for
  tested unchanged-reference/fee-state paths by entire-quote equality. Fee history
  remains a documented dependency, not an invariant frozen by EMA.
- H5: full-close certification uses live geometry — rejected: the shared EMA
  constructor and reference fee path are used; uncertain proofs refuse permission.
- H4: additional trading profit proves an accounting defect — not established.
  Exact settlement resource deltas are unchanged. The additional trading effect
  is real in the model and remains an open economic-risk disposition.

[Native results, assumptions and reproduction](../calibration/EMA_SETTLEMENT_CLOSEOUT.md)
include current fees, concentration, hLP, transfer fees, acquisition cost and
subsequent trading. Nothing here claims every possible route or configuration
has been evaluated, or that all of PR #45 received a new exhaustive review.
