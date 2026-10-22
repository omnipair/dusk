# Dusk audit status

**2026-10-18:** #45 is not ready to merge/deploy. The later model review's
hLP solvency/receivable finding remains open. Current follow-up fixes and
unverified candidates are listed in
[MODEL_REVIEW_CLOSEOUT.md](../../docs/audit/MODEL_REVIEW_CLOSEOUT.md).

**Current through:** 2026-10-17; runtime `5a9abcb3`, PR #45.

This is the current disposition summary. Dated reports describe their reviewed
revisions; their old zero-open counts are not current blanket clearance.

## Current disposition

| Original V12 unresolved subjects | Count |
| --- | ---: |
| Remediated reported root causes | 31 |
| Superseded old auction findings | 2 |
| #287160: mitigated, retained issuer/keeper risk | 1 |
| #287652: closed after remediation; residual inventory/arbitrage risk accepted | 1 |

The export had 169 reports. Its 134 Invalid labels were inventoried, not each
independently re-audited. Later NM-001–NM-005 are remediated. Governance delegation
review confirmed no new security defect; its Clippy build blocker was fixed in
`6da3f778`. The fresh EMA settlement closeout confirms no additional accounting
defect in its scoped review. The user accepted the remaining #287652 economic
tradeoff on 2026-10-17; the measured risk remains documented below.

- [Current findings and evidence](../../docs/audit/V12_FINDINGS.md)
- [Agreement coverage, checks and companion PRs](../../docs/AUDIT_CLOSEOUT.md)
- [#287652 current economics](../../docs/calibration/EMA_SETTLEMENT_CLOSEOUT.md)
- [#287160 fee-notice policy](../../docs/mutable-transfer-fees.md)

## Open / retained risks

**#287652:** internal settlement uses symmetric EMA price and smoothed depth,
not a caller-chosen live execution quote. Actual native tests preserve invoices
and settlement resource deltas, but preceding trades can still improve profit
from subsequent trading against the new real inventory. **Closed on 2026-10-17
with explicit user acceptance of that residual inventory/arbitrage risk.** The
V1 analogy describes the shared inventory mechanism, not identical profitability.
Principal shortfalls still use bounded insurance and write-off; later trading
losses receive no separate insurance draw. Zero loss/manipulation resistance is
not claimed. A caller minimum is not a protocol economic guarantee.

**#287160:** fee-aware admission, final-notice eligibility and keeper monitoring/
execution are implemented. A hostile fee authority or a missed profitable
execution window can still destroy collateral value. The loss waterfall handles
that loss; it does not prevent it. Fee-authority revocation is not an agreed
missing implementation. The new keeper candidate is not deployed.

Permissionless execution needs a caller, available funding/routes and successful
confirmation. Full cash exhaustion, proof limits and unprofitable residuals are
not eliminated by atomicity. No production readiness or universal liveness claim
is made by this ledger.

## Historical resolved mechanisms through 2026-08-29

The following table preserves the earlier review's evidence and accepted design
context. In particular, its liquidation-auction and direct-only governance text
describes historical implementations, superseded by #45's flash/emergency and
reusable governance balance interfaces above.

### Historical resolved findings

| Area | Historical finding aliases | Highest former severity | Current resolution and evidence |
| --- | --- | ---: | --- |
| Concentrated curve continuity, exact tails, maximal quotes, and split resistance | concentrated-AMM `FF-001`–`FF-007`; core `CORE-F01` | High | Replaced the discontinuous haircut and approximate boundaries with the shared exact curve and adjacent-atom maximality checks. See [`CONCENTRATION.md`](./CONCENTRATION.md) and `src/tests/math/concentrated.rs`. |
| Controller, hLP, leverage, and final-risk ordering | `CORE-F02`–`CORE-F05`; `SI-001`; older product/state hLP and stale-risk findings | High | Spot and all leverage swap families use the concentrated O(1) tail+band curve, algebraic zero-opposite-exposure hLP transition, protected recenter admission, and final risk observation. See `src/transitions/amm/mod.rs`, `src/transitions/amm/curve.rs`, `src/transitions/liquidity/hlp/integrated.rs`, and their tests. |
| Controller/config history, launch protection, and deferred work | `SI-002`; `CORE-F04`; older no-crank/config findings | Medium | Elapsed state is checkpointed under the old parameters before activation; saturated or invalidated work has explicit semantics and no maintenance instruction is required. Ordinary yLP can seed before `start_time`, trading activates at the exact timestamp, the launch fee decays from the clock in O(1), and a stateless launch-asset buy-size premium composes under the same hard fee cap. Both are governed through the fee family; no Alpha Vault, extra PDA, or launch keeper exists. Center-controller parameters use their own timelocked governance revision. |
| Bounded divergence computation | `CORE-F06` | Medium | Full-width fee math has bounded fallbacks and finished-SBF measurements. See [`COMPUTE_BENCHMARKS.md`](./COMPUTE_BENCHMARKS.md). |
| CPMM preview reserve-product overflow | `CORE-F07` | Low–Medium | Removed the unrepresentable raw `x * y` preview field. Preview now returns exact `floor(sqrt(x * y))` through full-width geometric-mean math; `preview_liquidity_supports_the_full_valid_reserve_domain` covers two maximum valid zero-decimal reserves. |
| Dead solver authorization surface | `CORE-F08` | Low | Production uses no finite-difference, Jacobian, Broyden, or iterative invariant solver. The implicit curve, frozen-cell scheduler, terminal-solver capability, and their obsolete fixtures have been removed. |
| Token-2022 mint identity and transfer accounting | older concentrated/token findings; `YN-01`, `YN-02` | Critical | LP hooks are immutable and bound to Dusk, market mints are pairwise distinct, and transfers checkpoint canonical owner accounts using measured token credits. |
| Yield identity, initialization, four-stream hLP accounting, and rounding | older product/state yield findings; `YN-03`–`YN-07`; auction/fee-dust finding 4 | High | Canonical idempotent accounts, pre-operation ownership, two-asset hLP revenue, exact aggregate carry, holder remainders, and partial direct-burn reconciliation have regression coverage. |
| Liquidation auction debt/lifecycle binding | older generic Feynman liquidation High; product/state stale-auction findings; auction finding 3 | High | Auction state is debt-side-bound and settlement first reconciles current risk, cancelling recovered positions before value moves. |
| Protocol-auction route and epoch isolation | auction findings 1 and 2 | High | Governance selects the exact route; epochs are isolated per market side, lane, and source, so unrelated or dust settlements cannot reset pricing. |
| Direct-yLP parameter governance | parameter-governance Feynman/State/Nemesis passes; event-reliability CI finding | CI / reliability | After canonical lazy hLP-vault initialization and CPI-event conversion, the governance passes verified no severity-classified defect. Rust and LiteSVM cover locking, virtual yield, strict majority, immutable actions, revisions, timing, rollback, and remint. The maximum proposal-create transaction is 980/1,232 bytes, leaving 252 bytes of headroom. |
| hLP reserve conservation | active-hLP exact-trace custody gap | High | Deleveraging cash spill is now recorded as source-scoped, non-executable backing inventory and released proportionally on hLP exit. Reserve custody covers cash, swap-fee custody, and both backing counters while tolerating unsolicited donations. Exact matched regressions account for the former 25,631-atom CPMM and 37,886-atom concentrated gaps. |
| hLP funding reuse and active-swap tracking loss | repeated cash-headroom reuse; post-only hedge | High | The concentrated integrated quote prices only ordinary yLP reserves, then reconstructs hLP yLP ownership and indexed funding debt algebraically at the quoted endpoint. Both active vaults finish with zero opposite-asset exposure; interest cash, virtual reserves, and token-vault settlement are reconciled once. The finished LiteSVM suite covers Spot in both directions plus leverage and liquidation. |
| Exhausted hLP terminal funding waterfall | passive hLP insolvency without terminal recovery | Medium | Added permissionless `close_insolvent_hlp`. It retires the exhausted vault's yLP ownership, draws only the borrowed-asset insurance balance, pays a bounded caller bounty from recovered funding interest, credits the remaining payable funding interest to yLP, and socializes only the caller-capped residual funding-interest loss. Any shortfall reaching principal fails closed. Existing hLP tokens remain burnable for zero principal while already-checkpointed fee claims remain separate. Focused rollback/accounting coverage and the final SBF/IDL build pass. |

### Historical accepted design context

| Area | Accepted behavior |
| --- | --- |
| Endogenous pricing | Dusk deliberately has no external oracle. EMA, controller, underwriting, and auction references are internal observations, not fair-price guarantees. |
| Liquidation capital and loss | Bidders or floor settlers provide debt assets externally. Residual insolvency can reach insurance and LP socialization; isolated-leverage residual principal follows its documented socialization path. |
| LP exit liquidity | Withdrawals remain cash-gated while assets are borrowed. The 80% governance execution guard is not a guarantee that every LP can exit. |
| Direct token burns and custody | Direct yLP burns are irreversible donations. A full direct hLP burn is deliberately fail-closed. SPL-multisig-owned LP custody is unsupported. |
| Fractional yield | Aggregate liabilities are exact, while unrelated holder accounts can each retain bounded sub-atom Q64 remainders rather than assigning them to another owner. |
| hLP funding-yield timing | hLP funding interest bears the full indexed cost and, after measured transfer credit and protocol splitting, is indexed only to non-hLP yLP present at the operation snapshot. Both hLP vaults are ineligible. A dedicated carry isolates this source from public interest and is cleared when only `MIN_LIQUIDITY` remains outside hLP. This is payment-time rather than accrual-time ownership, so ordinary yLP entering before settlement participates and ordinary yLP exiting beforehand forfeits participation. Permanently burned `MIN_LIQUIDITY` is the zero-holder sink; its backed allocation is unclaimable and cannot pass to a later depositor. Automatic deleverage sources the cost from the payer's burn legs, with exact target-side conversion for a borrowed-leg shortfall, rather than an additional shared-live debit. |
| Auction governance | Governance-approved route quality and retroactive terms for unsettled protocol-auction inventory are trust boundaries. Invalid routes fail closed. |
| Direct-yLP governance | Support is one-sided, queued support is frozen, metadata availability does not control typed execution, and direct burns can irreversibly reduce the eligible denominator. |
| Validated parameter domain | Arithmetic and curve proofs cover the enforced launch/governance bounds. Future bound expansion requires a new proof and benchmark pass. |
| Lazy risk materialization | Ordinary operations persist the required scalar observation; full pessimistic lending shapes are materialized by risk-sensitive paths. |
