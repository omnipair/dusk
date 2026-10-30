# Horus observation layer: standalone design contract

> Historical design. The later [OP-GAMM plan](../../docs/OP_GAMM_PLAN.md) supersedes the separate-market architecture and outage proposals below. The selected direction is one bounded family, with an explicit oracleless zero branch. Horus remains optional.

Status: research design, 17 October 2026; consumer delivery revised 18 October 2026. No Horus or Dusk program is implemented here. Dusk integration is deliberately held until [PR #45](https://github.com/omnipair/dusk/pull/45) is merged and the merged `main` can be inspected. This document is independent of the current dirty Dusk checkout.

## Decision in one sentence

The selected Dusk consumer interface is **one authenticated on-chain observation-account read**, with optional depth. Horus is one optional provider: Dusk's oracle-guided market type must not hard-code Horus as a privileged feed. Provider-specific report verification and aggregation happen before the read, potentially in earlier instructions of the same transaction. Dusk does not need a second raw-report verification API or a consumer-to-Horus validation CPI. End-to-end data age and transaction cost must be measured before choosing a production feed/update cadence.

## Three distinct boundaries

| Boundary | Delivery | Required behavior |
| --- | --- | --- |
| Provider to upstream oracle | Push account or submitted pull report | Horus identifies the exact feed, owner/program, instrument, source observation time, and verification status. |
| Upstream source to Horus | Explicit on-chain refresh or verified signed report | Horus reads already-supplied accounts/reports; a Solana program cannot fetch web APIs or order books itself. |
| Horus to Dusk | Authenticated observation-account read | Dusk checks the pinned provider/adapter, feed identity, version, source time, expiry and quality policy at the action. |

The cached path is Scope-like: an updater supplies source accounts, Horus validates them and writes a feed account. Scope documents this pattern as copying multiple on-chain oracle accounts into one validated feed: <https://github.com/Kamino-Finance/scope>. It does **not** establish that an upstream report is fresh simply because Horus's account was written recently.

The single read interface also permits this transaction composition, when the provider supports it:

```text
optional upstream pull-report submission
    -> provider/adapter verifies the report and updates its observation account
    -> optional Horus refresh validates/aggregates sources into its observation account
    -> Dusk reads its configured observation account and executes
```

All steps in that transaction succeed or fail together. This composition does not require an extra block between report verification and consumption, and does not change Dusk's account-read contract. If a market/action requires a transaction-local update rather than merely a sufficiently fresh account, that requirement needs explicit instruction/account binding; `last_updated_slot == current_slot` does not prove same-transaction execution. Do not claim that a cached read has this stronger property. Account count, compute, report size and account-write contention must be measured before declaring the composition feasible.

Reading an account does not itself add external-data latency: source publication, heartbeat/deviation settings, updater inclusion and any extra aggregation stage determine observation age. A stored feed still needs transactions to update it; its provider, an independent updater or the transaction builder may supply them. This selects one Dusk consumption method, not a promise that external data arrives without publishing infrastructure. Background refresh can improve availability, and just-in-time refresh can improve freshness through the same account interface.

The [OP-AMM paper](https://arxiv.org/html/2609.33799v1) compares push and pull update costs and notes that atomic consumption removes a separately observable on-chain update from an update sandwich. It does not prescribe a Horus-style aggregator or a particular oracle blend. A transaction-local report can still be erroneous, selected adversarially or late relative to the underlying market.

## Observation identity and immutable meanings

Each feed is keyed by a versioned `instrument_id`, not a free-form ticker. Registration binds the tradable mint, underlying or redemption contract, quote currency, scale/decimals, session calendar, and (where relevant) corporate-action or futures-roll methodology. A feed revision changes when any of these meanings changes. An observation carries:

- `schema_version`, `instrument_id`, `feed_revision`, `aggregation_policy_hash`, `report_sequence`, and digest;
- source event times, source receipt times when available, aggregation time, on-chain acceptance slot/time, and `valid_until`; timestamps have explicit units;
- reference price, executable bid/ask when evidenced, dispersion, source count, independent failure-domain count, source bitmap/commitment, and status flags;
- a separate depth-curve commitment and its own source times, quote expiry, route/access class and quality flags; and
- market-session state, next session transition, and whether the reference is for the underlying, token, or redemption claim.

Missing bid, ask or depth is **missing**, never zero. A price confidence band is not itself a liquidation-loss probability. On-chain accounts hold bounded summaries and commitments; raw venue snapshots and calibration evidence must remain retrievable and reproducible off-chain.

## Acceptance rules

1. **Authenticate each input.** For an on-chain source, check program owner, exact feed ID, instrument and exponent. For an off-chain depth report, verify authorized signer set, quorum, domain-separated signature, instrument, report/policy versions, expiry and strictly increasing sequence. Distinct signers observing one venue do not count as independent venues.
2. **Use source time, not write time, for freshness.** Reject future-dated observations outside clock tolerance, over-age constituents, expired quotes and bundles whose price/depth/session observations exceed a policy-defined maximum skew. A refresh must not extend the validity of stale constituents. Retain the oldest relevant source event time in the output.
3. **Require a unique agreeing price cluster.** Eligible sources are grouped by independent upstream failure domain, then compared in one currency and scale. A quorum and maximum dispersion are policy parameters; a tie or two conflicting quorums is invalid. The deterministic aggregate is a median of the selected independent observations, with fixed rounding. A quorum proves agreement, not correctness. Correlated-majority faults require separate stress testing and circuit-breaker policy.
4. **Construct depth by reachable venue/route, not by summing reporter totals.** Canonicalize venue and pool IDs; deduplicate shared pools and overlapping routes; cap a centralized route by accessible balances/credit and settlement eligibility. For competing reports of the *same* venue, select the policy's lower supported sell proceeds and higher supported buy costs rather than adding them. Store discrete size knots only within evidenced sizes; cumulative sell proceeds must rise with size with non-increasing marginal proceeds, and buy costs must rise with non-decreasing marginal costs. Reject invalid curves instead of silently repairing them.
5. **Separate evidence classes.** Mark observed cancellable book depth, executable firm/bonded quotes, and modeled future recovery as different types. A forecast includes model version and horizon; it cannot be substituted for current executable depth. Dusk's own reserves are not independent external liquidation depth.

The initial policy should be a **versioned, publicly inspectable template per instrument class**, with explicit source sets, age/skew limits, agreement tolerance, depth route set and session behavior. Templates are defaults, not permission to create a market. Market creators may choose another valid policy and bear its disclosed risks. Numeric limits require shadow-data calibration for any recommended template; this design intentionally does not turn the SOL research fixture's 180-second age or 100-bp cluster into a stock-market default.

## Consumer states and risk behavior

| Horus state | Stored-feed read | Price-sensitive new exposure or swap | Existing exposure |
| --- | --- | --- | --- |
| Healthy price and depth | Available with provenance and ages | Read the authenticated account under the action's freshness policy; optionally refresh it earlier in the transaction | Normal policy |
| Healthy price, unavailable depth | Price remains visible; depth is `missing` | Consumer may quote only under its explicit no-depth policy; no invented liquidity credit | Revalue/close under explicit policy |
| Authenticated disagreement, stale source or unacceptable feed status | Last accepted observation and its original time remain inspectable | Reject new borrowing, enlarged leverage, unsafe collateral release and ordinary oracle-guided swaps | Preserve direct repayment/add-collateral paths; test the prolonged-outage policy below for existing debt |
| Scheduled market closure | Follow the instrument's declared session policy | No assumption that the last open-session price is executable now | Use separately disclosed overnight/weekend terms rather than treating every close as an unexpected outage |
| Recovery | Candidate observations are visible but marked recovering | Re-enable only after an explicit re-entry rule; reset neither losses nor exposure budgets | Continue accounting through the outage |

Horus reports **observations and quality**, not a universal "safe price." Inventory-only fallback belongs to the consumer policy. If Dusk later permits bounded swaps during an outage, it must track persistent token budgets, cash/equity, realized losses and outstanding claims; inventory `q` alone can return to zero after profitable attack cycles. Fallback is not evidence for new leverage, loans or an indefinitely live market.

## Permissionless markets and interface separation

**Anyone may create a Dusk OP-GAMM market** under the separate oracle-guided market type, subject only to universal mechanical validity checks and funding/account requirements. No Horus administrator, oracle-provider approval, curated feed list or protocol proposal should be required to create one. The creator chooses and discloses the feed/policy, source set, sensitivity, inventory rule and other market parameters. LPs and traders decide which markets to use; a front end may label or curate markets, but its list is not an on-chain admission gate. This is the permissionless market model the design must preserve.

**Provider neutrality is not absence of oracle trust.** A market that prices from an off-chain reference necessarily depends on its selected provider's correctness and availability. Dusk can pin the selected adapter/program and feed identity per market, validate a common observation envelope (instrument and currency, scale, observation time, expiry, status and policy version), reject malformed/stale inputs, and isolate losses to that market. It cannot prove the external market's true price from an on-chain schema. The chosen adapter must verify its provider-specific account ownership, feed ID, signatures and report semantics; the market creator and LPs accept the remaining provider/model risk.

The proposed Dusk-facing interface is an `OracleObservationV1` account envelope, **not a claim of an existing Solana-wide standard**. It should carry a compact price observation with optional, separately typed directional depth. Both direct-provider adapters and Horus can implement it. Market initialization pins adapter identity, canonical feed/account binding and compatible interface version; execution cannot substitute a different adapter/account merely by passing different remaining accounts. Native provider layouts may need an adapter to produce this envelope. Dusk reads the authenticated account and applies its own acceptance rules; upstream verification is not duplicated in a separate Dusk report API. An oracleless market does not use this interface.

Horus should likewise permit **permissionless feed instances** with immutable identity and initial verification policy. A custom feed is not automatically endorsed: its source accounts, depth reporters, quorum and failure domains must be visible and mechanically verified. A permissioned publisher can be one *chosen* source type, not a universal condition for listing a market. Shared recommended feeds and policy templates may be maintained through their own governance without controlling unrelated feeds or markets.

A market pins the Horus feed identity and accepted policy/schema version range at creation, or explicitly opts into a documented per-market update mechanism. A Horus policy revision must not silently change an existing market's economics. Market-specific governance can later change oracle sensitivity within declared bounds through a prospective proposal, if that capability is built into that market type. Horus does not select the Dusk AMM blend, interest rate, inventory response, exposure budgets or loss-bearing terms. Updater identity never grants authority to choose a favorable source subset or change policy during a refresh.

Initial instructions to prototype, subject to SVM feasibility checks:

- `register_feed`: permissionless creation of a feed instance with immutable instrument identity and initial source/verification policy; optional governance is scoped to that feed instance.
- `stage_policy` / `activate_policy`: versioned changes by that feed's designated authority, with transparent delay and prospective activation; markets do not auto-adopt an incompatible revision.
- `refresh_cache`: permissionless *candidate* update from allowlisted on-chain source accounts; rejected updates leave the last accepted data and its original validity unchanged.
- `submit_depth_report`: verify signer quorum and sequence, then store a bounded depth summary and data commitment.
- The mock consumer reads the canonical observation account after these instructions; there is no second Dusk-facing `validate_for_consumer` CPI in this revision. Prototype enforcement of any same-transaction update policy without introducing a second consumption path.

"Permissionless" here is a proposed Horus and Dusk design property, **not** a statement about Scope's current refresh authority. The program still rejects malformed or unauthenticated data. A caller cannot pick only the favorable subset of a feed's required sources, replay a report, or overwrite a more recent valid report with older data.

## Verification before a Dusk integration

1. Implement the bounded account layout and deterministic integer aggregation in an independent Horus program/repository; compare native and SBF outputs at rounding and overflow boundaries.
2. Benchmark the single consumption interface with a previously updated account and with upstream report verification plus account refresh earlier in the consumer transaction. Include direct-provider and Horus adapters, at least three sources for aggregation, optional depth, and a mock consumer. Record source-to-execution age as well as compute, account, transaction-size and write-contention limits.
3. Test stale-write laundering, report replay, subset selection, duplicate venue depth, source disagreement, correlated-majority bias, session close/reopen, recovery flapping and failed transaction rollback.
4. Replay the existing optimized repeated-attack and funded-hLP fixtures with real Dusk accounting after PR #45 merges. The current research replay (historical local artifact: `analysis/oracle-guided-amm-2026-10-16/README.md`; not included in this implementation bundle) is not a proof of the paper's full OP-CFMM or a production calibration.
5. Only then specify the Dusk market account/interface changes on merged `main`; keep oracleless and oracle-guided markets separate, as previously agreed.

The earlier observation contract (historical local artifact: `analysis/autonomous-risk-pricing-2026-10-12/oracle-contract.json`; not included in this implementation bundle) is the broader research input inventory. This document narrows it to Horus delivery and verification semantics; it does not claim that all research fields fit in one on-chain account.

## Prolonged oracle outage: candidate for existing debt

Status: initial outage-policy experiment completed (historical local artifact: `analysis/oracle-outage-deleveraging-2026-10-18/README.md`; not included in this implementation bundle); still not an implemented or calibrated liquidation policy. This responds to the 18 October design discussion: freezing new risk cannot by itself unwind old borrowing and leverage. The paper's swap-pricing construction does not settle this credit-policy problem. The primary candidate is a **precommitted, time-progressive reduction in permitted debt**, paired with an actual repayment/execution route. Automatic inventory-only repricing remains a comparator, not the selected fallback.

### Time changes credit terms, not the claimed market price

Define `tau` as the duration of the authenticated unexpected outage, `g` as its repayment grace period, and `w` as the subsequent wind-down interval. For an illustrative linear rule:

```text
m(tau) = 1                              for tau <= g
m(tau) = max(0, 1 - (tau - g) / w)       thereafter
outage_LTV(tau) = pre_outage_liquidation_LTV * m(tau)
permitted_debt = remaining_collateral * last_accepted_debt_per_collateral_price * outage_LTV(tau)
```

All quantities must be put in consistent debt-token units, including fees and interest. Bind the baseline liquidation threshold to the position's disclosed pre-outage terms, rather than recalculating it from a manipulable live reserve signal during the outage. An existing position above this declining limit becomes eligible for the specified partial unwind. At the end of the interval, all remaining debt is callable: this does **not** assert that it has been repaid or that buyers exist. Actual debt and interest accounting are unchanged; the haircut is not a fabricated current price, realized P&L, increased debt balance, liquidation bonus or insurance credit. This is an outage recall term, not evidence that a position is insolvent at the external market price. The chosen price must have been accepted before the outage; the rejected price is never the recall reference. A last accepted price can itself have been wrong, which remains a failure case to test.

Example, ignoring fees and interest: a position owes 600 debt tokens and holds collateral worth 1,000 debt tokens at the last accepted ratio. It is below an 80% threshold, but above a later 50% threshold. An external repayment of 100 debt tokens restores compliance at that instant. A collateral sale requires a larger calculation because the sale also reduces remaining collateral; at the unchanged price, a 200-token sale/repayment reaches 50% LTV. At worse executable prices, partial sales may fail to restore health. Do not implement liquidation sizing as merely `debt - permitted_debt` when collateral funds the repayment.

### Execution and Dusk accounting

- Direct payment in the debt token and adding acceptable collateral remain available. A leverage close that swaps collateral through an oracle-guided AMM is not automatically safe merely because that trader reduces leverage: it can transfer an unpriced loss to LPs.
- Test small, externally funded auction lots. Bidders pay actual debt tokens for collateral; settle against net token credit, reduce actual debt, preserve borrower residuals, and apply explicit incentive and loss-allocation rules. The outage haircut determines eligibility, not the auction's execution price.
- Competition alone does not prove a fair price. The execution floor, allowable discounts, maximum lot size and no-bid/timeout behavior must be specified and tested before using this mechanism. An inflexible floor can stall; an aggressive discount can transfer value to bidders or encourage engineered outages. There is no price-free guarantee of both fair borrower treatment and timely LP repayment. Neither a timer nor a liquidation eligibility flag creates buyers.
- In the inspected local checkout, lending auction fills (historical local artifact: `programs/dusk/src/instructions/lending/liquidation/fill_liquidation_auction.rs`; not included in this implementation bundle) already receive actual external debt-token payments, although their reference pricing and eligibility still need adaptation. Leverage liquidation (historical local artifact: `programs/dusk/src/instructions/leverage/liquidate_leverage_position.rs`; not included in this implementation bundle) instead prepares a Dusk AMM swap. An outage exit for OP-GAMM leverage therefore needs a separate external-repayment settlement path with correct debt shares, interest, collateral custody, hLP accounting and loss allocation. Revalidate these observations against merged main before implementation.

### Outage identity, recovery and parameters

The outage must be determined from canonical configured feed state and chain time, not from a caller submitting a malformed account or rejected report. Merely receiving a bad candidate must not invalidate an otherwise valid canonical observation. Preserve the last accepted source timestamp and outage epoch; repeated stale writes, rejected reports and position splits must not restart the clock. Authenticate both sides of a price ratio and bind the policy version.

Scheduled equity/commodity closures require separate disclosed session terms, not a generic weekend liquidation timer. Chain halt/resume also requires a policy for the period during which borrowers could not act. Recovery requires sustained acceptable observations under a predefined rule; a single fresh timestamp must not reset repeated outages. Recheck recovery and position eligibility before auction fills, cancel no-longer-required unsold lots under the predefined rule, and do not undo completed fills or realized losses.

Research parameters are maximum source age, grace duration, wind-down duration/shape, partial-close limits, auction execution constraints and recovery duration. They must be fixed or changeable only under the market's disclosed prospective governance terms. No numerical settings are selected here. This price-outage candidate is separate from the optional depth-based **borrow-interest** premium; missing depth alone must not be treated as a missing reference price or silently activate this recall policy.

Compare freezing alone, bounded inventory fallback and timed recall under the same initial books/debt. Include a moving market during feed silence, stale-but-rewritten data, bad candidate reports, price recovery/flapping, engineered outages, no bidders, scheduled closures, chain restart, two debt directions and deteriorating execution prices. Measure actual debt retired and time to execution, LP bad debt/P&L, borrower execution losses and unnecessary liquidations, attacker profit, and hLP/debt-accounting conservation. Lock the account-read interface now; select the outage economics only after this comparison.

### Outage experiment outcome (18 October)

The 792-run comparison (historical local artifact: `analysis/oracle-outage-deleveraging-2026-10-18/README.md`; not included in this implementation bundle) uses retained public SOL books, synthetic failures and an independent GA pool/loan ledger. In 24-hour outage cases, timed recall with competitive external bids repaid essentially all initial debt, versus about 2.6% for inventory fallback. Timed recall did not reduce realized credit losses versus freeze in the base sample: both had zero loan writeoffs, and recall forcibly reduced otherwise healthy positions. No external bidders meant no recall repayments; a sole bidder paying the tested auction floor captured substantial borrower value. The simple timer remains a candidate, but the tested discount/timing is not selected.

Next specify and test actual external repayment/settlement and borrower execution protection, including no-bid behavior, before selecting a recall schedule. Do not turn this result into an automatic inventory fallback or claim native Dusk/hLP validation. The account-read interface, provider neutrality, separate market types and integration hold remain unchanged.

### External repayment prototype (19 October)

The standalone integer settlement prototype (historical local artifact: `analysis/external-repayment-2026-10-19/README.md`; not included in this implementation bundle) now implements funded debt-token payment, a fixed borrower execution floor, borrower surplus, exact policy/context/position binding, live eligibility checks, revision/deadline protection and a cumulative lot cap. The timer changes permitted debt, never the execution discount. Direct repayment needs no price. No admissible bid leaves the loan outstanding; no fallback or writeoff occurs merely because the timer expires.

The same 264 scenario/cohort/direction combinations were run under the three original controls and three fixed floors (1,584 runs). At the illustrative 2% setting, sole-bidder borrower execution cost falls from 42,214 to 4,184 USDT per 200,000 opening debt, with outage repayment falling from 100% to 97.42%. Evening six-hour repayment falls from 92.36% to 83.48%; a tail case repays only 1.36%. Base terminal LP marked losses match freeze, so this does not establish lower realized credit losses. Fixed policies also add a 10% minute lot cap; the comparison does not isolate the floor's effect alone.

The enforceable protection is against the **last accepted reference**, not the unknown current market price. A rally can still make a floor-compliant sale very cheap; a crash can stop repayment entirely. No discount setting is selected for deployment. Next map this external-payment contract onto debt shares, collateral custody and hLP accounting **after PR #45 merges and merged main is reviewed**. The prototype does not implement or validate any Dusk runtime/IDL/SDK change.

### Forty-eight-hour borrowing and faster margin decay (19 October)

The 1,200-run schedule experiment (historical local artifact: `analysis/time-decay-2026-10-19/README.md`; not included in this implementation bundle) compares 48-hour borrowing, six-hour margin and twelve-hour margin decays after a common one-hour grace. It holds the loan portfolio fixed to isolate timing. With competitive bids and a fixed 2% stale-reference execution floor, mean outage repayment is 85.61%, 98.20% and 96.55%, respectively, but individual crash cases remain blocked. Mechanically reducing the sale-price floor too allows approximately full repayment under competitive bidding.

Weak competition defeats that latter rule: an immediate sole bidder paying the declining floor repays approximately all debt but leaves less than 0.01 USDT residual borrower equity in every public-data case. A bidder waiting until 90% of the decay has elapsed repays only 14.76% for the 48-hour schedule and 13.12% for the six-hour schedule, leaving roughly 167,000–170,000 USDT mean unsecured debt per 200,000 opening debt. Flat-price synthetic tests also show the equity drain. These are modeled participation strategies, not observed oracle attacks or calibrated failure probabilities.

Keep collateral-recognition decay as a candidate **credit recall term**, distinct from the actual collateral sale price. Neither the tested fixed floor nor the unbounded declining floor guarantees timely, fair execution. Borrower equity loss and lender shortfall must be reported separately; collateral disposal is not debt repayment. The Dusk integration hold, provider neutrality and single authenticated observation-account read are unchanged.
