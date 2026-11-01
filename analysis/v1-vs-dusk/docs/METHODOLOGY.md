# Methodology contract

This document describes the implemented Dusk-versus-V1 historical
counterfactual protocol. It is a preregistered analysis contract, not a result.
No numerical finding should be inferred until all immutable stages complete
and the final manifest verifies.

## 1. Questions and unit of comparison

The factual lane is native Omnipair V1 state reconstructed from finalized
history. Counterfactual lanes feed the same canonically ordered user-flow
evidence into Dusk's native integer transition code. The benchmark asks:

1. **Collateral efficiency:** at the same observed fraction of V1 admissible
   borrowing capacity, how much gross/actionable debt can V1 and Dusk support,
   how much target debt is filled, and what becomes unmet?
2. **Spot capital efficiency:** for factual swaps and immutable standardized
   probes, how do executable output, execution cost, slippage, CPMM-equivalent
   depth, fee return, and utilization of factual capital differ?
3. **Leverage:** at an exact paired market checkpoint, how do theoretical and
   executable leverage, initial/maintenance margin buffers, borrowing/funding,
   adverse-path breach, and full unwind compare?
4. **LP and protocol economics:** how do swap fees, public-borrow interest,
   hLP funding, recenter inventory, and V1-only lanes differ after separating
   factual user flow, synthetic arbitrage, synthetic protocol settlement,
   accrual, cash realization, and non-revenue balance-sheet movements?

The benchmark never treats a Dusk quote, counterfactual success, current token
price, or derived indexer estimate as a factual observation.

## 2. Immutable identity and cutoff

`RunContext` hashes the exact `benchmark.toml` bytes, a finalized cutoff slot,
and source bytes from this benchmark, the Dusk program, the V1 program, and the
relevant indexer decoder/core trees. The resulting run ID is
`slot-<cutoff>-<16hex>`. Dirty and untracked source bytes are intentionally
included; generated data, `.env`, `.git`, and build output are excluded.

A configured nonzero cutoff is used as written. If the cutoff is zero, one
finalized slot is resolved and frozen in
`results/runs/cutoff-<config-hash-prefix>.txt`. A later stage must present a
verified predecessor receipt with the same run ID, config hash, source hash,
cutoff, and prior receipt hash. `--force` can replace a verified blocked stage
receipt after its cause is corrected; it cannot replace a completed receipt or
bypass a validation gate.

## 3. Factual sources

### PostgreSQL snapshot

The indexer export uses `DATABASE_URL` at runtime and begins a read-only
`REPEATABLE READ` PostgreSQL transaction. The checked-in SQL exports market
metadata, event seeds, coverage counts, and only real historical hourly price
marks. Database ingestion timestamps are never chain time. Raw token/share
quantities remain canonical base-10 integer strings.

### Finalized archival RPC

The RPC lane inventories program signatures and fetches every scoped block with
`transactionDetails=signatures`. The persisted complete ordered signature list
proves transaction indexes and binds the canonical block header/time. Each
selected target payload is then fetched independently with finalized
`getTransaction`; request and exact raw-response hashes are retained for both
lanes. Top-level `meta.rewards` is removed as the sole declared normalization
because the canonical full-block request sets `rewards=false`; nested rewards
and every other transaction/meta field remain exact. A deterministic 128-slot
sample is also fetched with full-payload `getBlock`, and extraction fails unless
headers, ordered indexes, status, transaction, and normalized metadata match
exactly. The legacy full-payload path remains isolated for selected-mint
lifecycle reconstruction. RPC work is local-only, checkpointed, retry-bounded,
and resumable without rewriting remote state.

Historical account state at the terminal cutoff is an independent import, not
an extraction-stage product; Section 7 defines it.

The checked-in hybrid panel still inventories successful program signatures,
but block download targets are selected-market DB signatures plus successful
program signatures absent from every DB row. `cohort-representativeness.json`
binds selected versus visible-market swap, debt-action, and liquidation row
counts and exact basis points. This reduces block volume without changing any
selected-scope fetch/decode gate. It leaves one explicit methodological edge:
a transaction indexed only under a non-selected market could also carry a
global side effect and remain outside scoped blocks. Only block replay spanning
the complete visible market universe rules out that mixed-transaction case.

### Historical prices

USD metrics use one explicitly configured provider and the exact UTC hourly
bucket for each observation. Accepted rows must be historical and provider
sourced. Current, estimated, portfolio, EMA, forward-filled, and present-price
substitutions are rejected. Missing marks preserve eligible raw/invariant
metrics when otherwise complete, but make the affected market ineligible for
USD headlines.

## 4. Decoding, normalization, and total order

Every canonical event carries authoritative RPC block time and this Solana
execution order:

```text
(slot, transaction_index, outer_instruction_index,
 inner_execution_index, event_sequence)
```

The decoder also retains `execution_path`, matching the indexer's
`absolute_path` convention (outer instruction index followed by per-stack
sibling counters), plus CPI stack/frame provenance. This prevents assertions
or inner instructions from being attached by stack height alone.

Coverage is measured at one consistent natural grain: expected versus decoded
successful, state-changing target-program instructions. A failure in one
transaction is recorded with signature, slot, transaction index, expected
count, decoded count, and completeness; it does not discard valid sibling
transactions. Unknown instructions, incomplete CPI provenance, transaction or
block decode failures, normalization gaps, order duplicates, and missing chain
time are hard blockers.

Database and RPC records are reconciled on raw identities/fields and classified
rather than coerced. Any unmatched authoritative row, field disagreement, or
missing enrichment remains visible in reconciliation artifacts.

### Decoder revision gate

Real history requires non-overlapping program lifecycle slot ranges with the
exact deployed binary SHA-256, matching IDL/source identity, and an actual
revision-specific implementation. The checked-in registry has one provisional
all-slot decoder. Therefore it cannot pass
`decoder_registry_headline_verified`; it is suitable only for reconstructive
diagnostics. Merely marking it non-provisional would not establish historical
dispatch and would violate this contract.

AuditGrade requires this certification to pass. DecisionGrade may retain the
underlying failure only as the typed
`decoder_revision_certification` limitation after exact selected-scope
state-changing decode, source agreement, and native economic replay are proven.
No other decode failure is eligible for that limitation.

## 5. Token-program and transfer behavior

Each market mint is bound to its owning token program. Legacy Token produces an
explicit zero-fee epoch. Token-2022 requires a complete finalized signature and
block page chain through the cutoff, ordered lifecycle mutations, and exact
epoch activation semantics. Initialize/set-transfer-fee changes cannot be
collapsed into one schedule: initialization can begin at the current epoch,
updates activate after the protocol delay, pending updates can be overwritten,
and a closed/reinitialized mint address starts a new lifecycle.

Failed transactions remain coverage evidence but do not mutate mint state.
Missing pages, ambiguous terminal pagination, unsupported extensions, or an
active Transfer Hook are fail-closed blockers. Historical success cannot prove
that an unknown hook would accept a changed counterfactual gross amount.

Leverage eligibility never infers mint safety from a zero fee epoch. Each mint
decision binds its owner program, decimals, complete initialized extension set,
whether `TransferFeeConfig` ever existed, and a domain-separated decoded-mint
history SHA-256 to the sealed signature-page, block, and mutation evidence.
Legacy Token is eligible only within Dusk's decimal limit. Token-2022 is
eligible only when its complete extension set and collateral-fee status satisfy
the exact native Dusk guards; an unknown extension set or incomplete history is
typed unavailable and fails closed.

## 6. Data-quality gates and market universe

Raw eligibility requires complete scoped finalized transaction inventory, exact
state-changing decode coverage, canonical order and RPC time, versioned config
coverage, DB/RPC agreement, complete applicable transfer-fee history, no
unsupported hook, and later exact native V1 replay. Headline
USD eligibility additionally requires the configured historical-price and TVL
coverage. Coverage thresholds in configuration do not override zero-tolerance
identity, ordering, state-changing decode, transfer-fee, replay, or source gates.
AuditGrade additionally requires historical decoder-revision and terminal-state
certification. An exact DecisionGrade hybrid panel may carry only those two
underlying failures as typed final-gate limitations.

The universe begins with visible markets. A market must pass hard gates, have at
least 10 factual swaps, and have swaps on at least 3 distinct RPC-time UTC days
under the checked-in policy. Tail exclusion uses historical **median
time-weighted TVL**, not an activity score:

1. Form the population of visible, hard-gate-passing, activity-passing markets
   with complete historical time-weighted TVL marks.
2. If fewer than 10 markets qualify, define no TVL tail and remove none.
3. Otherwise sort exact median TVL ascending and select the value at
   `floor(n / 10)` for the configured p10 policy.
4. Exclude only markets strictly below that threshold; retain ties at the
   threshold.

A market without valid historical TVL remains available for raw/invariant
analysis if its raw gates pass, but cannot enter the collective USD headline.
It is never ranked by `swap_count × active_days` or by current price.

The collective common window ends at the frozen cutoff and is derived from the
configured market and factual TVL fractions (currently 90% of markets and 95%
of TVL), not hardcoded dates. The runner writes
`universe-window.json`, `common-window.json`, and
`chronological-splits.json` only after the configured post-replay final gate
authorizes the evidence tier.

## 7. Native V1 replay and terminal certification

Canonical operations are replayed through the native V1 engine with versioned
configuration and exact transfer behavior. The replay ledger must satisfy zero
raw-unit difference for every reproduced transition and all conservation
identities.

Transition self-consistency is necessary but insufficient. An independently
sourced `terminal-binding.json` must bind run, cutoff, V1 program, sanitized
provider identity, and the complete sorted account list. Its paired
`terminal-snapshot.json` must bind the raw provider response and contain every
account's owner, lamports, executable flag, rent epoch, data encoding/data, and
raw-data hash plus a full account-set hash. The response context slot must equal
the cutoff exactly; `minContextSlot` is not proof of historical state.

The importer decodes the required V1 Pair, RateModel, FutarchyAuthority,
UserPosition, and token-vault accounts and compares terminal raw units to native
replay checkpoints. Missing, extra, duplicate, wrong-owner, wrong-slot,
wrong-program, malformed, or unequal accounts hard-fail. The retained terminal
reconciliation is sanitized; binding, account list, raw bytes, owners, and
wallet/position details remain under the ignored `data/<run-id>/` tree.

Terminal equality is mandatory for AuditGrade. If both independent terminal
inputs are absent, an otherwise exact DecisionGrade selected-scope replay may
continue only when the final gate records `terminal_state` with underlying
status `fail` and certification `limited`. The failure is not converted into a
pass, and reports must display it prominently. A partial input pair or an
unequal supplied snapshot always fails.

## 8. Chronological calibration and holdout

After the common window is derived, factual operations, V1 replay, price, and
transfer-fee inputs are materialized as hash-bound chronological prefixes. The
checked-in split is 60% training, 20% validation, and 20% sealed holdout. Split
boundaries are derived and persisted, not hand-selected.

V1 `interest_bps` is a surcharge on native LP interest, while Dusk's protocol
interest parameter is a share of gross borrower interest. They are never copied
as if they had the same denominator. `calibrate` derives a self-hashed
`protocol-interest-schedule.json` from the physically sealed training operation
and factual-ledger prefixes only. Each exact market/config epoch retains token-
side borrower, nominal-protocol, cash-covered-protocol, and fitting-error totals
without adding unlike token atoms. The fitted Dusk share exhaustively searches
the protocol-valid integer bps domain with deterministic lower-bps ties.

An epoch with no positive training accrual seals
`training_fit_available = false` and the deterministic nominal conversion
`surcharge / (10000 + surcharge)`, again with lower-bps ties. Such an epoch may
support spot or hLP-only evaluation rows, but any positive factual public-
interest accrual under it fails closed. Evaluation must resolve the exact
market/config ID and order interval: an unseen post-training config ID fails
even when its numeric bps happen to equal a frozen epoch.

One protocol-wide shared Dusk parameter profile is explored and locally refined using only
training and validation. Per-market opportunity ceilings are separately
qualified and may not silently replace the collective comparison. The chosen
shared profile—or the Pareto frontier when no feasible winner exists—is frozen
in schema-version-2 `profile-freeze.json`, which commits to the schedule hash
and input/source hashes before holdout evaluation can read holdout data.

## 9. Counterfactual replay lanes

The runner evaluates two swap interpretations:

- **fixed flow:** preserve factual gross exact-in token flow; and
- **intent:** preserve decoded historical min-out intent, with counterfactual
  arbitrage tracked separately from user flow.

Borrowing preserves each borrower's observed fraction of V1 admissible
capacity. If Dusk permits more capacity, the counterfactual target can increase
at that same fraction; a collateral-only event never invents debt. The metric
ledger reports gross capacity, actionable cash capacity, target/fill/unmet
amounts, and resulting debt separately.

Both the maximum-feasible hLP upper-bound arm and the no-hLP control run. No arm
may introduce external capital, free rebalancing, or unobserved wallet actions.
Every Dusk row has exactly one of three mutually exclusive revenue origins:
factual user flow, synthetic arbitrage, or synthetic protocol settlement. The
two synthetic origins are labeled separately and excluded from user-flow
revenue. A dual-flagged row is invalid rather than assigned by precedence.

## 10. Spot efficiency and standardized returns

Each paired operation row can carry an immutable spot-probe set. Probe sizes are
derived only from factual V1 observations inside the exact scored chronological
split: 10, 100, and 500 bps of factual pool NAV, plus factual gross-input p50
and p90 per direction. The sizing window, frozen NAV-bps vector, sizing-source
hash, and probe-set hash are persisted. V1 and Dusk quote the same gross amount
from their exact pre-state. Results retain token transfer fees, curve
input/output, LP/protocol/dynamic fees, executable status, gross and wallet-net
quote improvement, execution cost against one common factual V1 reference,
venue-relative invariant slippage, and CPMM-equivalent depth per exact external
LP NAV. Missing aggregate Dusk owner state or rejected hLP settlement is
unsupported evidence, not a zero-depth observation.

Absolute revenue returns divide exact economic income by factual V1 accounted
LP capital-time exposure. This is a standardized common-capital comparison: it
does not assert who owned Dusk external-LP or hLP capital. APR is shown only for
windows at least 30 days long and is labeled as annualized; shorter windows
retain absolute return only. Exact Dusk external-LP-owner and hLP-owner APY are
typed `missing_ownership_denominator` until native owner-claim capital-time
evidence exists. Passive LP inventory PnL is likewise explicitly unsupported
until ownership/NAV provenance exists; neither quantity is imputed.

## 11. Isolated leverage

`benchmark_leverage_exact` compares Dusk's native isolated leverage open/close
with V1's executable margin-swap, deposit, borrow/swap/deposit looping path at
one exact paired checkpoint. It reports theoretical and executable leverage,
opening costs/liquidity limits, instantaneous funding, initial and maintenance
buffers, standardized and source-bound realized adverse excursions, and exact
unwind constraints.

The `evaluate` stage writes one self-hashed
`leverage-campaign-<profile-id>.json` for each shared profile. For every
USD-headline lending market, its grid contains exactly 72 cells:

```text
3 first/median/last checkpoints
× 2 long-base/long-quote directions
× 3 starting margins (10, 100, 500 bps of factual accounted pool NAV)
× 2 replay arms (fixed-flow, intent)
× 2 hLP scenarios (no hLP, maximum feasible)
= 72
```

Each cell binds the selected canonical operation/order, profile, replay arm,
hLP scenario, native pre-state commitments, mint-history eligibility proof,
and exact same-checkpoint unwind horizon. The core theoretical/executable
leverage, opening, instantaneous funding, initial/maintenance buffer, and
immediate full-unwind metrics are therefore comparisons of the same captured
checkpoint; later factual liquidity is never invented.

Future-path claims have a separate availability lane. Until a separately owned
future Dusk position/state branch exists, the campaign records
`missing_exact_dusk_future_counterfactual_state`, keeps
`complete_for_headline_path_claim = false`, and projects those claims as typed
N/A. It never substitutes the factual V1 marginal-price path for native Dusk
future closeout values. A missing exact pre-state, mint proof, admissible
starting margin, or native transition also yields a typed blocker rather than a
zero. USD-headline markets without factual lending activity are explicitly
not-applicable and do not contribute leverage cells or aggregate leverage
claims.

The available per-cell `LeverageBenchmarkResultV1` serializes with schema
version 2; the enclosing campaign uses schema version 2 and binds the exact V1
replay certification policy. Campaign hashes and
source-artifact bindings feed the result manifest's exact per-market and
aggregate leverage metrics. This integration is an analysis contract only; no
real leverage result is committed by this repository.

## 12. Revenue, safety, and decision rule

Token-side evidence is an exhaustive 15-lane cube for each applicable
venue/origin: LP swap; protocol fee-auction and buyback swap; retained recenter
inventory; compounded LP swap principal; LP and protocol public interest
accrued; LP, protocol fee-auction, and protocol buyback public-interest cash;
external hLP funding plus protocol fee-auction and buyback cuts of that
funding; V1-only flashloan fees; and V1-only protocol claims. Lanes retain an
accounting class: economic revenue, cash-realization memo, inventory memo,
principal-disposition memo, or venue-only supplement.

Stakeholder economic headlines sum only the economic-revenue lanes. They
include LP swap, LP/protocol public-interest accrual, net hLP funding to the
external LP side, and the protocol hLP cuts. They never double count
cash-realization memos, retained/recenter inventory, compounded principal,
claims, or flashloan supplements. External-LP combined economics include LP
swap, LP public-interest accrual, and net hLP funding; protocol hLP cuts remain
only in protocol and all-stakeholder components.

V1 has no exact LP public-interest cash-realization witness, so that lane and
its return component remain `missing_cash_realization_evidence`. Native V1
cash-covered protocol interest is exact cash evidence and is assigned wholly
to the protocol fee-auction lane; V1 has no supported buyback split, so its
buyback lanes are structurally not applicable. These cash lanes remain memos
and never replace economic protocol-interest accrual.

Historical USD aggregation preserves exact arbitrary-precision rationals until
an explicit signed-integer reporting boundary. Every nonzero lane fails its own
USD total if either required historical token mark is missing; zero atoms do
not require a price. Only missing marks in economic-core lanes block the
stakeholder headline and candidate score. Missing marks for cash, inventory,
principal, claim, or flash diagnostics keep those lanes unavailable without
silently suppressing an otherwise complete economic score. A shared-profile winner is allowed only
if bad-debt, tail liquidation-shortfall, volume-weighted execution-cost, and
p95-slippage gates pass. Objective comparisons include stakeholder revenue,
quote quality, and borrow capacity with deterministic profile-ID tie-breaking.
If no profile passes, the output is a Pareto frontier and the reports must say
that no shared-profile winner was established within the bound evidence scope.

## 13. Holdout sensitivity

The sealed holdout ledger is resampled in paired UTC seven-day blocks with a
fixed seed and sample count. All economic lanes share one sampled-block path;
the artifact commits to that path hash. Stakeholder user-flow revenue delta,
synthetic-arbitrage revenue, and synthetic protocol-settlement revenue remain
three separate outputs. Quoted execution-cost sensitivity is emitted only when
every required paired observation is supportable.

Top-market exclusion ranks exact factual USD swap input and factual stakeholder
revenue descending, with ascending market identity as the deterministic tie
break. Missing provider marks, source hashes, or identity bindings fail rather
than falling back to current prices or partial rankings. The serialized
sensitivity artifact is schema version 2, is self-hashed, and its exact
rational structures are included in the schema-version-3 result manifest's
`sensitivity_evidence`.

## 14. Verification, reports, and schemas

`metrics-manifest.json` schema version 3 binds the run, shared profile,
Dusk/source identity, universe, holdout, market qualifications, source
artifacts, exact metrics, accounting, winner state, and sensitivity evidence.
Its scope evidence additionally binds the scope mode, evidence tier, exact
selected-market set/hash, cohort-selection hash, selected-versus-visible
representativeness for swaps/debt actions/liquidations, and final phase-gate
decision/hash. Only `decoder_revision_certification` and `terminal_state` may
remain as typed DecisionGrade certification limitations; AuditGrade requires
both underlying gates to pass. All source, decode, native replay, metric,
accounting, conservation, price, and coverage gates remain mandatory for both
tiers. `verify` runs the Rust manifest validator, re-hashes every safe relative
source path, and executes the source-backed notebook with the standard-library
runner. It writes only `verification.json` and
`benchmark.executed.ipynb` for that stage.

Historical DB rows can reference a market no longer present in the current
catalog. Only a DecisionGrade hybrid panel whose complete target set is fixed by
explicit overrides may quarantine this evidence. It preserves every such row
from the full window with a dedicated checksum, separately reports the
training-prefix subset, and excludes the unresolved addresses from visible-
market ranking because visibility and mint bindings cannot be inferred. The
same condition is fail-closed for AuditGrade, full-universe, or dynamically
filled panels.

`render` requires the verification receipt and the portable builder path named
by the configured process environment variable (currently
`V1_VS_DUSK_REPORT_BUILDER`). It produces three views over the same verified
manifest:

The configured project wrapper delegates to the official builder named by
`V1_VS_DUSK_CANONICAL_REPORT_BUILDER`. It changes no data or claims: it only
contains the reader header and root horizontal overflow before the official
1440 px and 390 px Chromium checks run.

```text
reports/generated/<run-id>/technical/...
reports/generated/<run-id>/governance/...
reports/generated/<run-id>/public/...
```

Each directory contains `artifact.json`, `provenance.sql`, `report.html`, and
`render-receipt.json`. Every quantitative claim must carry a metric ID and exact
numerator/denominator; presentation conversions are labeled. Wording is derived
from the verified winner state and must remain conservative. Hybrid-panel
reports label the result as a selected-panel/shared-profile result, show the
three representativeness percentages, and surface all typed certification and
methodological limitations prominently. Compatibility machine identifiers
containing `global_*` or `global_profile` mean the one protocol-wide shared
profile within the bound evidence scope; they never assert full-universe
evidence.

The JSON Schemas under `schemas/` are committed hand-maintained snapshots. They
are not generated by `verify`. Rust types and runtime validators remain
authoritative; `schemas/validate.py` checks catalog integrity, checksums,
references, strict object contracts, and named Rust symbols.

## 15. Retention and limitations

`data/<run-id>/`, all JSONL/Parquet, terminal inputs, parsed RPC evidence and
response hashes,
calibration prefix/cache directories, and environment files are ignored. The
module permits reviewed sanitized aggregate JSON, notebooks, SQL provenance,
render receipts, and reports to be retained. Unignored output is not
automatically safe: review source paths, market identifiers, and narrative
claims before commit or publication.

Known certification and methodological limitations at this revision are:

- the shipped decoder registry is provisional and cannot certify historical
  revision dispatch; AuditGrade fails closed, while exact selected-scope
  DecisionGrade may carry the typed `decoder_revision_certification`
  limitation;
- exact terminal binding/snapshot inputs require an independent historical RPC
  source and are not produced by the runner; AuditGrade fails closed, while
  exact selected-scope DecisionGrade may carry the typed `terminal_state`
  limitation;
- hybrid-panel block targeting can miss a global side effect in a transaction
  indexed only under a non-selected market; representativeness is disclosed,
  but only complete visible-market-universe block replay rules out this edge;
- unsupported Token-2022 hooks or incomplete mint history block affected raw
  and USD market eligibility;
- missing historical price/TVL marks block USD headlines rather than being
  approximated;
- passive LP inventory PnL is unavailable without ownership/NAV provenance;
  and
- exact future Dusk leverage-path claims remain typed N/A until a separately
  owned future position/state branch is available; same-checkpoint core
  leverage metrics remain independently auditable.

There is no valid benchmark conclusion until all gates required by the chosen
evidence tier pass and all seven receipts complete. A DecisionGrade conclusion
that retains either permitted certification limitation must remain explicitly
labeled limited; it is not an AuditGrade or full-universe verdict.
