# Versioned schema snapshots

This directory contains committed, hand-maintained JSON Schema snapshots for
the benchmark's stable machine-readable contracts. The catalog is
[`catalog.v1.json`](catalog.v1.json); its own contract is
[`catalog-v1.schema.json`](catalog-v1.schema.json).

The Rust `serde` types and runtime validators named by each catalog entry are
authoritative. These snapshots are strict interoperability contracts: object
schemas reject unknown fields, exact token amounts remain decimal strings, and
each snapshot is SHA-256-bound in the catalog. A disclosed
`x-validation-scope` identifies schemas that intentionally cover only an
envelope, selected metric structures, or the portable report-builder subset.

Neither `verify` nor any other runner stage generates these files. `verify`
checks `metrics-manifest.json`, its source-artifact hashes, and the notebook; it
does not compare or refresh schema snapshots.

## Validate the snapshot set

From `analysis/v1-vs-dusk`:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 schemas/validate.py --self-test
```

The pure-standard-library validator rejects duplicate JSON keys and verifies:

- every committed `*.schema.json` is cataloged exactly once;
- catalog IDs, validation scopes, and file SHA-256 values match;
- local and cross-file JSON pointers resolve;
- required-property declarations are internally consistent; and
- every artifact entry names a real Rust source file and symbol; and
- catalog-marked exact contracts have the same required field set as their
  Rust structs, including result sensitivity summaries and spot probe/yield
  structures.

This is snapshot-set validation, not runtime-artifact certification. Only the
named Rust validators can enforce self-hashes, ordered-ledger commitments,
provider identity, exact-slot RPC semantics, accounting identities, and
cross-artifact equality.

## Current coverage

The catalog covers canonical operations/events, real historical price marks,
terminal binding/snapshot imports, deterministic Parquet sidecars, paired
metric rows and summaries (including spot probes and all revenue lanes),
candidate score/capacity/revenue-return evidence, paired sensitivity evidence,
the training-only protocol-interest schedule, the profile freeze, the result
manifest, and report-builder specs. It snapshots the schema-v2 serialized
`LeverageBenchmarkResultV1` and
the strict schema-v2 `LeverageCampaignArtifactV1` now emitted by `evaluate`.
The campaign schema references the benchmark-result schema and covers every
versioned nested struct, including checkpoint selection, replay evidence,
mint-history eligibility evidence, typed blockers, and exact cells.

File/URN versions follow each serialized `schema_version`, not the Rust type
suffix. The current paired metric row/summary and aggregate candidate evidence
are cataloged as v5, sensitivity is v2, and the result manifest is v3, even
though their compatibility-oriented Rust type names still end in `V1`. Result
manifest v3 binds the evidence scope/tier, selected-market set, cohort
selection and representativeness, final phase-gate decision, and any permitted
typed certification limitations.

One campaign artifact is self-hashed per protocol-wide shared profile. Its intended complete
grid is 72 cells per USD-headline lending market (3 checkpoints × 2 directions
× 3 margin sizes × 2 replay arms × 2 hLP scenarios). JSON Schema validates the
strict serialized shape; Rust verification remains authoritative for the
self-hash, grid/identity invariants, same-checkpoint semantics, non-lending N/A
projection, mint-history proof binding, and typed future-path availability.

This is not yet a schema for every intermediate file. Stage receipts,
extraction/decoder inventories, transfer-fee provenance/registry, data-quality
and DB/RPC reconciliation artifacts, universe/split artifacts, and
counterfactual replay rows still rely on their Rust `serde` types, runtime
validators, and stage/source hashes. Add snapshots for those contracts when a
non-Rust consumer needs them; do not invent or infer shapes from sample output.

To change a stable contract, update its Rust type/validator first, revise the
snapshot and catalog entry, replace the catalog checksum with the SHA-256 of
the final schema bytes, then rerun `schemas/validate.py --self-test`. Do not
weaken a snapshot merely to accept an older artifact; add a new version.
