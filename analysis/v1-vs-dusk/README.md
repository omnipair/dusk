# Dusk versus V1 historical counterfactual benchmark

> **Archived controller model.** This runner and its committed schemas preserve
> the utilization-target Dusk model that existed when the campaign was designed.
> It is not compatible with the active coverage-based controller without a new
> benchmark revision. Do not use its target-utilization, curve-steepness,
> adjustment-speed, or rate-at-target fields as current Dusk configuration. See
> [the active specification](../../docs/BORROW_RATE_CONTROLLER.md).

This isolated runner is designed to replay Omnipair V1's finalized historical
activity as the factual control and apply the same ordered user-flow intent to
Dusk's native integer transitions. It measures collateral/borrow capacity,
spot capital efficiency, isolated leverage, LP economics, protocol economics,
execution cost, and safety without silently replacing unavailable history.

**Current status:** the implementation and validation contracts exist, but no
real benchmark result is committed here. The checked-in campaign is a frozen
five-market `hybrid_panel` with `decision_grade` evidence. It may produce a
selected-panel result only after exact scoped fetch/decode, source agreement,
native economic replay, metric, conservation, price, and coverage gates pass.
The provisional historical decoder certification and an absent exact-cutoff
terminal snapshot remain two explicitly typed limitations; `audit_grade` or
`full_universe` evidence still requires both to pass.

For the normative definitions and gates, read
[`docs/METHODOLOGY.md`](docs/METHODOLOGY.md). For machine-readable contract
coverage, read [`schemas/README.md`](schemas/README.md).

## Prerequisites

Keep these repositories in the layout expected by the checked-in path
dependencies:

```text
Omnipair/
├── dusk/
│   └── analysis/v1-vs-dusk/
├── omnipair-rs/
│   └── programs/omnipair/
└── omnipair-indexer/
    ├── api/.env
    └── indexer/
```

The run requires:

- a Rust toolchain capable of building the locked dependency graph;
- read-only PostgreSQL access to the indexer's V1 database;
- a finalized archival Solana RPC that can return every scoped block's complete
  ordered signature inventory, every independently selected transaction,
  complete program-signature and selected-mint history, and—when AuditGrade is
  requested—exact historical account context at the cutoff;
- real hourly historical marks from the configured provider (currently
  `birdeye`), never current or estimated prices;
- Python 3 for the standard-library notebook verifier/executor;
- Node.js plus an explicit portable Data Analytics builder script for `render`;
  and
- enough local storage for archival block JSON, JSONL, and Parquet outputs.

By default, `benchmark.toml` reads
`../../../omnipair-indexer/api/.env`. It expects the keys `DATABASE_URL`,
`SOLANA_RPC_URL`, and `OMNIPAIR_PROGRAM_ID`; process-environment values for
those same names override the file. Secret values are never copied into
manifests. PostgreSQL export runs in a read-only `REPEATABLE READ` transaction,
and no runner stage performs a remote write.

For a `hybrid_panel`, scoped target blocks are selected-market DB signatures
plus successful program signatures absent from every DB row. The run records
selected-versus-visible-universe swap, debt-action, and liquidation coverage.
A finalized signature-only `getBlock` response proves the complete transaction
order; only selected target payloads are fetched with finalized
`getTransaction`. Both request/response hashes and the complete ordered
signature list are persisted. Before accepting this composition, the runner
compares 128 evenly spaced slots against full-payload `getBlock` and requires
exact equality of headers, indexes, status, transaction, and metadata after
removing only top-level `meta.rewards`, which the full-block request explicitly
suppresses.
A transaction indexed only under a non-selected market could theoretically
also carry a global side effect and remain outside those scoped blocks; only
block replay spanning the complete visible market universe rules out that
mixed-transaction edge.

Review `benchmark.toml` before a campaign. Its paths are resolved relative to
the config file, not the invoking shell directory. The checked-in campaign
uses a fixed finalized cutoff. If `window.cutoff_slot = 0`, the runner resolves
one finalized slot and freezes it at
`results/runs/cutoff-<config-hash-prefix>.txt` before extraction.

## Evidence tiers and unresolved certifications

### Historical decoder dispatch

[`decoder-revisions.toml`](decoder-revisions.toml) currently contains one
all-slot, `provisional = true` revision. It is useful for reconstructive audit
evidence only. `extract` can persist evidence and `audit` can produce a
data-quality artifact, but `decoder_registry_headline_verified` remains false.
AuditGrade fails closed. DecisionGrade may retain this as
`decoder_revision_certification` only after the selected scope proves exact
state-changing decode, source agreement, and native economic replay; it never
rewrites the underlying failed gate into a pass.

Before a real run, replace that placeholder with non-overlapping lifecycle slot
ranges bound to the exact deployed program-binary SHA-256 and the matching IDL
and source identity. Each range must dispatch to its real revision-specific
decoder. A hash in configuration without real historical dispatch is not
sufficient.

### Exact terminal checkpoint

No CLI stage fetches or constructs these two independent inputs:

```text
data/<run-id>/terminal-binding.json
data/<run-id>/terminal-snapshot.json
```

`reconcile-v1` first proves native transition replay and writes its transition
reconciliation. AuditGrade fails closed if either terminal input is missing.
An exact DecisionGrade selected-scope replay may continue only with a typed
`terminal_state` limitation in the final phase gate. When supplied, the binding
must identify the same run, cutoff, V1 program, sanitized provider identity,
and complete sorted account list. The snapshot must return
owner, lamports, executable flag, rent epoch, and raw account data with hashes,
with the RPC response context slot **exactly equal** to the cutoff.
`minContextSlot` alone is insufficient; latest/current-state substitution,
missing accounts, extra accounts, duplicates, and raw-unit differences are
rejected. These inputs are wallet-level and remain gitignored.

For an AuditGrade campaign, once the independent inputs are present, rerun the
blocked stage with `--force`. `--force` permits replacement of that stage's verified **blocked
receipt**; individual artifact writers still enforce their own atomic/resume
rules. It never replaces a completed receipt or relaxes a gate.

## Run the immutable stages

Run from this directory. The predecessor receipt must be complete and bound to
the same config, source tree, cutoff, and run ID.

```sh
cargo run --locked -- extract
cargo run --locked -- audit
cargo run --locked -- reconcile-v1
cargo run --locked -- calibrate
cargo run --locked -- evaluate
cargo run --locked -- verify
V1_VS_DUSK_CANONICAL_REPORT_BUILDER=/absolute/path/to/deliver_portable_artifact.mjs \
V1_VS_DUSK_REPORT_BUILDER=./scripts/deliver_portable_report.mjs \
  cargo run --locked -- render
```

Every command accepts `--config PATH`; append `--force` only after correcting a
recorded blocker so its blocked receipt can be replaced. The builder variable name is configured by
`output.portable_report_builder_env`. It is read from the **process
environment**, not from the indexer `.env`; an absolute script path is the
least ambiguous choice. `output.node_executable` selects the Node executable.
The checked-in wrapper delegates validation, packaging, static-chart extraction,
and desktop/mobile verification to the canonical builder. Its only deterministic
HTML adjustment contains the canonical reader's full-viewport header and clips
root-level horizontal overflow so classic-scrollbar Chromium builds do not add
an eight-pixel page scrollbar.

The run ID has the form `slot-<cutoff>-<16hex>` and commits to the config bytes,
cutoff, and source identity. The source identity fingerprints the benchmark,
Dusk program, V1 program, and relevant indexer source bytes; dirty and
untracked source changes therefore produce a different run ID.

For this fixed-override DecisionGrade hybrid panel only, DB events whose market
address is absent from the current market catalog are retained for the entire
window in `cohort-catalog-unbound-db-events.jsonl` with their own checksum and
excluded from visible-market ranking. The artifact separately counts the
training-prefix subset. These rows are never silently promoted to hidden or
visible markets. Catalog-unbound history fails closed for AuditGrade,
full-universe, and dynamically filled panels.

Stage responsibilities are:

1. `extract` — export the read-only database snapshot; crawl finalized program,
   block, mint, transfer-fee, and real historical-price evidence; enrich chain
   time/order; decode/normalize; and write deterministic JSONL/Parquet inputs.
2. `audit` — reconcile database and RPC evidence and emit hard-gate,
   configuration, price, decoder, transfer-fee, CPI, and per-market coverage.
3. `reconcile-v1` — run the native V1 replay, require zero raw-unit differences
   and conservation, apply the configured final certification policy, derive
   `universe-window.json`, `common-window.json`, and
   `chronological-splits.json`, then materialize frozen split inputs.
4. `calibrate` — derive and seal the per-market/config protocol-interest share
   schedule from training prefixes only, search and refine one shared Dusk
   profile on training and validation, write the Pareto/winner evidence, and
   freeze the schedule hash and selected profile before the holdout is
   accessible.
5. `evaluate` — replay the sealed holdout arms and hLP controls, write paired
   metric ledgers, exact spot/capacity/revenue evidence, one self-hashed leverage
   campaign per shared profile, deterministic sensitivity artifacts, and the
   self-hashed `metrics-manifest.json`.
6. `verify` — verify the result manifest and every named source hash, then run
   `scripts/execute_notebook.py` against `notebooks/benchmark.ipynb`; it writes
   `verification.json` and `benchmark.executed.ipynb`. It does **not** generate
   schemas.
7. `render` — require the verification receipt, write three source-backed Data
   Analytics specs/provenance files, invoke the caller-supplied builder with
   Node, and verify each HTML/render receipt.

## Output layout

All output roots below are config-relative:

```text
data/<run-id>/
  blocks/...
  canonical-events.jsonl
  canonical-operations.jsonl
  historical-prices.jsonl
  terminal-binding.json                 # independently supplied; private
  terminal-snapshot.json                # independently supplied; private
  parquet/...

results/runs/<run-id>/
  01-extract.receipt.json ... 07-render.receipt.json
  data-quality.json
  cohort-selection.json
  cohort-representativeness.json
  v1-final-phase-gate.json
  v1-reconciliation.json
  v1-terminal-reconciliation.json       # optional DecisionGrade; required AuditGrade
  universe-window.json
  common-window.json
  chronological-splits.json
  protocol-interest-schedule.json
  profile-freeze.json
  paired-metrics-<profile-arm-hlp>.jsonl
  candidate-evidence-<profile-arm-hlp>.json
  leverage-campaign-<profile-id>.json
  sensitivity-<profile-arm-hlp>.json
  metrics-manifest.json
  verification.json
  benchmark.executed.ipynb

reports/generated/<run-id>/
  technical/{artifact.json,provenance.sql,report.html,render-receipt.json}
  governance/{artifact.json,provenance.sql,report.html,render-receipt.json}
  public/{artifact.json,provenance.sql,report.html,render-receipt.json}
```

Raw history, per-operation JSONL/Parquet, terminal inputs, and wallet-level
artifacts are ignored by Git. Sanitized aggregate JSON, verification outputs,
and generated reports are retainable only after human privacy/source review;
being unignored is not an endorsement or publication decision.

Each leverage campaign covers the 72-cell grid for every USD-headline lending
market: 3 first/median/last checkpoints × 2 directions × 3 factual-NAV margin
sizes × 2 replay arms × 2 hLP scenarios. Core opening, funding, margin-buffer,
and immediate-unwind metrics compare exact native states at the same captured
checkpoint. A missing exact future Dusk position branch remains typed N/A for
future-path claims; it is never replaced with the factual V1 path. Markets
without factual lending activity are explicitly not applicable. Every native
eligibility decision binds the decoded mint-history proof used by Dusk's mint
guards.

Revenue output preserves 15 token-side lanes for each applicable venue and
origin. The three origins are factual user flow, synthetic arbitrage, and
synthetic protocol settlement; a row cannot belong to more than one. Economic
accrual is never merged with cash realization, retained inventory, compounded
principal, flash fees, or protocol claims. Every nonzero lane needs its own
historical USD mark to publish that lane's USD total. Missing price evidence on
a non-economic memo lane does not suppress an otherwise complete stakeholder
economic headline, but that lane remains typed unavailable.

Absolute returns use one common factual V1 accounted-LP capital-time
denominator so venue economics are comparable without inventing external Dusk
capital. APR is emitted only for windows of at least 30 days. Exact Dusk
external-LP-owner and hLP-owner APY rows remain
`missing_ownership_denominator` until native ownership capital-time evidence
exists. V1 protocol public-interest cash is exact only for the native
cash-covered amount and is assigned to its fee-auction lane; V1 LP-interest
cash realization remains explicitly unavailable.

Validate the committed schema snapshot set independently with:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 schemas/validate.py --self-test
```

This benchmark is a historical counterfactual, not a forecast, deployment
claim, or production launch gate.
