# OP-GAMM implementation plan

Updated 31 October 2026. The design originated on 26–27 October and is now integrated on merged PR #45, including its #52 changes. This document preserves the design decisions and planned validation sequence. The implemented contract, current validation and limitations are recorded in [implementation status](OP_GAMM_IMPLEMENTATION_STATUS.md).

Proposed direction: one permissionless GAMM family that retains Dusk's lending, leverage, fungible LP ownership and hLP. λ = 0 runs without a price oracle; an authorized proposal can later activate λ > 0 in any market. Its accounting must remain consistent when the reference changes without a trade. Horus is an optional provider, not a prerequisite or privileged dependency.

The [coupled derivation and exit-loan checks](OP_GAMM_COUPLED_MODEL.md), [bounded adjustment](OP_GAMM_BOUNDED_ADJUSTMENT.md), and [frozen/carry reproductions](OP_GAMM_FROZEN_CARRY_REPRO.md) retain their research assumptions and dated results. The implementation now connects the bounded pricing kernel to instruction authentication, swaps, lending, leverage, hLP, governance and SDK builders. See [implementation status](OP_GAMM_IMPLEMENTATION_STATUS.md) for the current validation results and remaining limitations. This is a pre-deployment account layout; no live-account migration is implied.

## Decisions to carry forward

| Area | Contract |
| --- | --- |
| Market creation | Anyone can create a GAMM, subject to mechanical validity and funding requirements. The same creation instruction accepts an optional oracle policy: omission selects native λ = 0; supplying one selects the initial source and weight. Every market supports later oracle activation or changes through governance; there are no immutable creation-time activation bounds. First liquidity validates the live feed for positive weight. No provider approval or curated feed list is required. Keep distinct markets' balances, claims and losses isolated. |
| Oracle consumption | One authenticated on-chain observation-account read. A provider may verify a report and update that account earlier in the same transaction. Dusk does not fetch off-chain data or expose a second raw-report verification API. |
| Provider choice | The creator pins the adapter/program, canonical account, instrument and policy revision. Direct-provider adapters and Horus use the same consumer contract. Provider neutrality does not eliminate the chosen provider's correctness, availability or upgrade-authority risk. |
| Oracle weight | The deployer chooses λ within the implemented family's validated bounds, including an explicit native branch at zero. There is no universal 50% setting. Proposals may change it prospectively under the market's disclosed rules, with a coherent debt/ownership/risk transition; automatic outage-driven changes are excluded. |
| Depth | Optional, separate **borrow-interest** pricing. No depth means ordinary base APR. Valid depth can add a nonnegative risk spread. Use a genuinely asymmetric filter: fast deterioration, slow recovery. This is not an added spot-swap fee or automatic increase in permitted leverage. |

Use the existing shared accounting kernels and one pricing family with an explicit native branch. At λ = 0, price-sensitive operations must not require a price account, price refresh or freshness validation. An independently enabled depth-interest extension has its own disclosed data dependency.

This supersedes the earlier requirement to create a separate pool and move liquidity. Once the layout and governance capability exist, an allowed in-place activation need not move LP inventory. It must preserve physical balances, principal/shares, funded claims and risk history, and explicitly handle existing positions. A timelock does not guarantee exit liquidity. Adding this capability to code that lacks it still requires a program/account-version change; there are no production markets or production-account migration requirements here.

## Current code baseline

[PR #45](https://github.com/omnipair/dusk/pull/45) merged on 29 October 2026. Implementation is based on merged main `d3d1cfadfcd36d34ada911dd31f7d588df8c78e6`; its tree matches the research pin `77877d576018fd2bd8191d13f05534b0484e2282`. PR #52 was already incorporated into #45. The clean baseline passed 544 native Dusk tests. Refresh the base and rerun affected checks if main advances before publication.

The current branch matters in three ways:

1. **Ownership includes accrued public interest, while execution remains cash-constrained.** hLP settlement, funded yield, advances, canceled interest and terminal receipt cohorts must survive the extension. Public receivables are not executable liquidity.
2. **hLP already has maintenance without swaps.** New-income settlement and external flash recovery are implemented. The current external recovery path accepts a solvent funding gap or frozen hedge; it does not require the old 12.5% critical threshold. Recovery still needs cash, a transaction and a willing counterparty.
3. **Economic time is now slot-derived.** Oracle source timestamps, publication age and scheduled venue closures need separate semantics. Reusing a slot checkpoint as proof of fresh external information would be incorrect.

The [idle-market experiment](../analysis/hlp-idle-oracle-2026-10-20/REPORT.txt) used older commits and changed only the recovery reference. Its results motivate the tests below; they do not certify the current branch or a full OP-GAMM. Re-run the accounting and cadence controls after the merge. Do not carry forward its old recovery threshold or quantitative outcomes as current behavior.

Current source anchors: [LP accounting](https://github.com/omnipair/dusk/blob/51be3578704eddbbeb2671723f3d1f357bf9a985/docs/LP_INTEREST_ACCOUNTING.md), [hLP flash eligibility](https://github.com/omnipair/dusk/blob/51be3578704eddbbeb2671723f3d1f357bf9a985/programs/dusk/src/transitions/liquidity/hlp/flash.rs), [economic time](https://github.com/omnipair/dusk/blob/51be3578704eddbbeb2671723f3d1f357bf9a985/docs/ELAPSED_TIME.md).

## 1. Specify the pricing and ownership equations now

Use the paper's geometric blend as the control and its bounded extension as the next candidate:

```text
log P_OP = λ log P_reference + (1 − λ) log P_native
P_OP     = P_reference^λ × P_native^(1 − λ)
P_bound  = P_native × exp[clip(λ log(P_reference/P_native), −log M, +log M)]
```

M is the maximum multiplicative adjustment relative to the native price. Both paths take the native branch explicitly at zero. The bounded curve preserves the continuous native tail's non-depletion property; clipping occurs in the marginal pricing rule before integration. Hard clipping introduces a derivative kink. Its numerical treatment and M bounds remain design inputs. The tested M = 2 and λ = 0.5 are examples, not selected deployment settings.

Both prices use the same token units. `P_native` means the marginal Dusk curve price under explicitly frozen geometry and accounting coordinates, not the raw reserve ratio. Concentration, center state and hLP ownership make extending this equation a substantive derivation.

The [paper](https://arxiv.org/html/2609.33799v1) supplies a geometric family and a conditional trading invariant, but no universal optimal λ. Its framework constrains inventory and oracle responses. Its generic geometric construction also has a unit-invariance caveat outside the CPMM baseline. Dusk must explicitly prove its normalization and concentrated extension; the paper does not cover Dusk's credit and hLP accounting.

Produce five concrete outputs:

1. **Frozen-state trading rule:** integrate the marginal price into exact-input and exact-output settlement. Derive behavior across every concentration boundary and at inventory limits. Multiplying the old swap output by an oracle adjustment is not an implementation of this rule.
2. **Reference-update rule:** changing the oracle alone moves the price function, not physical balances, debt shares, LP ownership or funded yield. The conditional invariant level is recomputed from the same inventory; its numerical change is not earned income. Any later ownership conversion must balance explicit receipts.
3. **Ownership rule:** solve ordinary LP and both hLP claims together with the OP endpoint. Preserve public-interest backing, segregated funded claims, frozen vaults and retired cohorts. Measure transfers between LP groups rather than hiding them in aggregate NAV.
4. **Center and fee rule:** define how the native concentration center participates in the blend, including deferred center moves and their protected funding buckets. Oracle movement is not customer volume. External returns, inventory displacement and execution fees need distinct signals so one oracle move is not charged or accumulated twice.
5. **Numerical contract:** specify λ bounds, normalization, rounding, overflow, approximation error and maximum solver work. Use λ = 0 as a native-equivalence test, intermediate weights as experiments, and values close to 1 as stress cases. Do not promise the current analytic quote path survives the extension.

Keep the semantic prices explicit:

| Quantity | Meaning |
| --- | --- |
| External reference | The accepted observation, with source identity, age and quality. It can differ from a token's redeemable or executable value. |
| Swap price | The OP curve integrated over trade size, followed by the specified fees and transfer accounting. |
| LP/hLP ownership valuation | The rule used to allocate inventory and funding debt. It must be consistent with the OP settlement, without creating claims from repricing. |
| Credit health reference | Admission derives from the same OP market price and its conservative recovery model, with explicit size, depth and exposure constraints. Liquidation eligibility may use a different filter of that market reference. An independent raw-oracle mark must not bypass admission constraints. |
| Recovery execution | Actual tokens received and paid, with fixed session obligations, incentives and borrower/LP protections. |

These quantities can differ by design. Each difference needs a stated reason and reconciled value transfer. Feeding an external price into recovery while leaving incompatible native ownership conversions in place is not sufficient.

### Borrowing remains coupled to the market

Clarification, 27 October 2026: separating price roles does **not** authorize pricing swaps from inventory while granting loans against an unrelated, more favorable external mark. Borrow capacity must remain tied to the market's collateral/recovery economics. Here, borrowing valuation means how much debt collateral supports, not the borrow APR.

In [PR #45 at `77877d57`](https://github.com/omnipair/dusk/blob/77877d576018fd2bd8191d13f05534b0484e2282/programs/dusk/src/transitions/lending/mod.rs), `current_risk_at_slot` derives observations from the current concentrated spot. Admission uses the lower symmetric/directional price reference and a size-sensitive borrowing shadow curve backed only by the full-range tail, with conservative depth. Cash, collateral-factor and aggregate exposure checks impose further limits. Liquidation eligibility uses the symmetric EMA; it is not identical to instantaneous spot or to the eventual execution price. These paths were checked against the research pin, whose tree matches the merged implementation baseline above.

For OP-GAMM, derive admission observations from the OP curve and specify its conservative recovery model. Do not independently replace collateral valuation with `collateral × raw oracle price`. External information may inform that model, but cannot justify bypassing its cash, size or exposure constraints. Test purchase → collateral deposit → borrow → abandon sequences in both directions, including false/stale oracle values, token/underlying basis, temporary liquidity and λ changes. Matching a marginal spot alone does not prove exit-loan resistance, and today's admission test cannot guarantee future liquidation proceeds.

At λ = 0, the candidate swap formula and clean hLP equations reduce to native behavior, with the native admission operator and no independent raw-oracle lending mark. The proposed live zero mode explicitly has no price-feed dependency. Reaching it from a positive λ still requires a governed transition that preserves accumulated risk history and outstanding market terms. Exact native integer behavior with accrued receivables, fees and frozen hLP remains a required differential test.

The mathematical checks distinguish common oracle-error losses from an added lending loophole. A false reference can make both loans and direct sales profitable; in the displayed single-position cases, direct sales pay more. Repeated loans that ignore existing exposure do create extra exit capacity. Preserve the full admission contract, including the projected market-health floor: the bounded study's source-derived continuous calculation passed its fixed-price and split-position controls with that floor included. This is finite evidence, not a global recovery theorem or native-runtime certification.

**Completion test:** high-precision reference equations, boundary cases and an independent token/claim ledger agree before production integration begins.

## 2. Specify oracle accounts and market policies now

Reuse the [Horus consumer design](../analysis/horus-2026-10-17/README.md). Proposed account names and layouts remain provisional until the math and compute review.

Pin a compact, versioned observation envelope and per-market binding. In positive-λ mode, authenticate owner/program, exact feed, token pair, denomination, source time, sequence, expiry, quality status and policy revision at every price-sensitive operation. Zero mode must bypass price-feed validation explicitly. A report freshly written on-chain may contain stale source data. Optional bid/ask and depth have independent validity; absence is not zero and confidence is not executable spread.

The first adapters should include a deterministic test provider and one real price provider. Horus aggregation can progress independently. An arbitrary permissionless adapter may publish false data; pinning it makes the trust choice explicit, not objectively correct. Disclose upgrade authority and reject silent incompatible feed-policy changes.

Freeze the accepted observation and policy revision for each operation and flash obligation. SDK builders may prepend report verification/refresh through the same account-read interface. Same-slot timestamps alone do not prove same-transaction refresh. Test report selection, ordering, stale-write laundering and updates between flash begin and settle.

The following price-feed action policy applies when λ > 0; λ = 0 does not become unavailable because an unused price feed is stale.

| Price state | Planned action policy |
| --- | --- |
| Valid | Permit actions under their ordinary cash, margin and execution checks. |
| Stale, invalid or disputed | Reject new borrowing, enlarged leverage, unsafe collateral release and ordinary OP swaps. Continue accrual. Preserve direct debt-token repayment and collateral additions that need no invalid conversion. |
| Prolonged outage | Preserve debt and borrower claims. An external close needs actual repayment and explicit execution protection. No automatic inventory fallback or collateral-price decay toward zero. The execution policy remains an unresolved design item. |
| Scheduled closure | Apply disclosed session rules. A closing print is not a live tradable price. Define reopening gaps and the token/underlying basis separately. |
| Recovering | Require a specified re-entry condition. Refresh current balances and revisions; do not reset losses, funded claims, debt, or cumulative limits. |

A leveraged close can reduce the borrower's exposure while harming LPs through a stale swap. Therefore, "risk reducing" is not an exemption from execution validation. Price-free proportional withdrawals, if supportable, need an accounting proof rather than a blanket permission or pause.

For prolonged outages, previous tests established the limitation: a stale execution floor can block crash repayment, while a descending floor can drain borrower equity. Select a bounded execution contract before wiring forced stale-price liquidation. Borrower-authorized external routes are useful but cannot guarantee permissionless recovery of an absent borrower. A timer does not supply price information, bidders or cash.

**Completion test:** account schema, action matrix, session/calendar semantics and adversarial mock-provider tests are reviewable independently of Dusk integration.

## 3. Specify the complete economic prototype

Full implementation is now authorized. The prototype and integration must include the bounded OP curve, joint LP accounting and both debt directions; the earlier recovery-reference-only comparison is insufficient. PR #45 has merged, satisfying the original integration dependency.

Compare native Dusk, direct-feed OP-GAMM and aggregated-feed OP-GAMM with matched starting capital, exogenous customer intents and funding assumptions. Derive arbitrage and executions from each model. Matching total volume would conceal the mechanism being tested. Borrower acceptance remains an explicit sensitivity assumption until quote/acceptance logs exist.

| Test family | Required cases and measures |
| --- | --- |
| Price movement and execution | Historical stress, jumps, differing oracle latency, stale reports, spread/basis and venue closure/reopening. Measure trader slippage, fill rate, fees, external arbitrage transfer, LP returns and usable leverage under the specified policy. Crypto books do not calibrate stock or commodity markets. |
| Repeated attacks | Optimize timing, direction and size across update sandwiches, biased fresh feeds, correlated aggregation failures, λ changes and recovery cycles. Preserve cash, exposure and loss history across cycles. Inventory returning to its start is not a zero-loss test. |
| Idle hLP | No swaps with moving prices, funding accrual, hourly versus end-only touches, exhausted income cash, weak/absent buyers and resumed trading. Compare hedge drift, recovery delay/cost and each LP group's net return. |
| Ownership and entry/exit | Oracle-only updates, deposits/withdrawals around updates, fee compounding, receivable repayment/cancellation, terminal cohorts and frozen hLP. Reconcile all claims, custody and debt; separate marked value from withdrawable cash. |
| Depth interest extension | With depth: asymmetric deterioration/recovery, spoofing, depth loss, exposure and execution delay. Without depth: base APR. Compare incremental risk pricing against an equally priced flat interest premium, including demand sensitivity and LP losses. |

Preserve trade-time hLP rebalancing. Reuse new-income settlement and external recovery for idle maintenance before adding another instruction. Specify who submits a call, the source of its payment and the maximum incentive. A feed update does not execute Dusk, and a maintenance call cannot spend unbacked receivables.

For optional depth pricing, specify whether and how the spread affects public debt versus internal hLP funding before integration. Define prospective floating-rate accrual: checkpoint elapsed debt under the old rate before accepting a new spread. Depth outages must not reset the spread to zero or stop interest. Existing depth-market policy blocks enlarged exposure while invalid; price-based repayment/liquidation can remain independent. EMA half-lives and APR mapping remain calibration outputs.

**Completion test:** no unexplained accounting gain or deterministic repeatable round-trip credit at a fixed observation; economic losses under inaccurate feeds are measured and disclosed. Report hLP funding, external execution costs, maintenance/oracle costs and ordinary LP income separately to avoid double counting. Predeclare comparison criteria before choosing a recommended λ/fee profile.

## 4. Integrate on merged main

Re-check #45's merge, pin main, run its existing baseline checks and port the experiment to that exact code. Reassess the current full-value solver, flash eligibility, cohort lifecycle and slot-time behavior before interpreting old results.

Suggested review sequence, with each change independently testable:

1. **Oracle configuration and validation:** disclosed market capability and λ bounds, optional oracle configuration/state, provider binding and reusable action validation. Favor an optional sidecar if it avoids burdening zero-mode execution; benchmark the extra account before fixing the layout.
2. **OP trading kernel:** implement the derived curve and bounded numerics through the shared prepared-swap path. Wire exact-in/out, previews, fee collection, concentration/controller revisions and joint hLP reconstruction together.
3. **Credit and hLP lifecycle:** integrate admission, health, internal/external settlement, direct repayment, income maintenance, terminal accounting and cohort rotation under the documented price roles. Retain measured net-credit and fixed flash-obligation protections.
4. **Governance and SDK:** add bounded λ proposals, prospective activation, revision invalidation and snapshot binding. Regenerate all four Dusk/delegate SDK interface files. Expose current mode, allowed λ range, activation authority, reference/price age, quote bounds and failure reasons to builders and previews.
5. **Operational integration:** update indexer/events and transaction builders, then maintenance and recovery callers. Exercise composed oracle refresh + swap/close/flash transactions under real account, heap, compute and packet limits. Include the optional depth-interest module after its rate contract is specified.

Primary code surfaces are `state/market.rs`, market initialization, `transitions/amm/{curve,swap,fees}.rs`, `transitions/liquidity/hlp/`, lending/leverage and flash settlement kernels, `transitions/governance.rs`, instruction account validation, and `packages/dusk-sdk`. These are integration boundaries, not instructions to duplicate each subsystem.

λ changes must settle elapsed accounting under the old policy, validate the new reference and bounds, invalidate prepared quotes/caches/deferred targets, and activate atomically with a new revision. Changes can create trading opportunities and transfer value even without changing account layouts. Test the entire transition; a timelock alone is not economic protection. Do not retroactively rewrite stored position margin terms or debt.

## 5. Verify the complete market

Keep lending, leverage, hLP and ordinary LP behavior in scope throughout. If a feature reveals an unresolved design conflict, surface that conflict rather than silently removing the feature.

Required evidence:

1. Native-market differential tests, OP boundary/property tests and independent claim/cash reconciliation, including zero-fee repeated cycles and λ = 0 controls.
2. Deterministic economic replay on the merged baseline, with resumed trading, long idle gaps, adverse oracle ordering and parameter transitions. Show cohort returns, borrower outcomes and execution availability, not just aggregate LP P&L.
3. LiteSVM tests for authentication, account substitution, token fees, malformed/future/replayed observations, begin/settle binding, rollback and actual composed routes. No assumed compute headroom from the old AMM.
4. Full interface regeneration and the complete local CI sequence required by `AGENTS.md` and the current workflow before each commit; SDK preview-to-event parity and indexer gross/net reconciliation included.
5. Focused adversarial review of oracle-update value transfers, hLP ownership, parameter activation and outage execution. Any accepted residual risk must be stated for the chosen market policy; neither aggregation nor the paper proves zero LVR.

## Work order and open decisions

**Now:** finish the compute-bounded integer trading kernel, explicit net-NAV/backing/carry representation and prospective λ-transition contract on the merged baseline. Complete the integration sequence above and all required checks before opening the full PR. The instruction is to hold publication until that work is complete; the frozen-accounting correction and docs must not be presented as the finished feature.

Planning estimates for one engineer/researcher: 3–5 working days for the math/account contract, 5–10 for the complete prototype and adversarial replay, and 10–20 for integration plus runtime/interface validation after the merge. These exclude independent review and new licensed equity data; an unsuitable concentrated invariant would require revising the estimate.

The remaining decisions are concrete:

| Decision | Proposed direction | Evidence needed |
| --- | --- | --- |
| Concentrated OP curve | Bounded normalized correction as candidate, original geometric blend as control, native curve at zero. | Select cap/kink numerical treatment; prove full-state hLP endpoint and bounded integer compute; retain center accounting. |
| λ policy | One family; native zero default for every market; governed activation/deactivation and policy changes within the implementation's mathematical limits. | Both-direction transition attacks, existing-position treatment and full native equivalence; no default 50% assumption. |
| Idle hLP maintenance | Keep in-trade rehedging; reuse income and external flash recovery. | Latest eligibility, finite cash, incentives, cadence and weak-buyer tests. |
| Prolonged outage execution | Price-free repayment stays open; no automatic inventory or zero-price liquidation fallback. | A chosen execution/protection tradeoff for existing debt; no claim of guaranteed fair repayment without fresh information. |
| Optional depth interest | Base APR plus a calibrated nonnegative spread with fast-down/slow-up depth memory. | Rate-accrual contract, scope of charged debt, executable depth evidence and demand/LP-loss comparisons. |

**Completed design inputs:** [coupled model](OP_GAMM_COUPLED_MODEL.md) and [bounded adjustment comparison](OP_GAMM_BOUNDED_ADJUSTMENT.md). **Current implementation input:** [runtime feasibility and remaining work](OP_GAMM_IMPLEMENTATION_STATUS.md). The mathematical checks and isolated compute probes do not certify full transaction admission or production attack resistance.
