# Benchmark Data Index

Run: `slot-440459364-06c0e05aadb7e883`
Cutoff slot: `440459364`
Verdict: the row-level V1 evidence is complete; the comparative Dusk score is unavailable.

## Actual datasets

| Dataset | Rows | Grain | Files | Quality |
|---|---:|---|---|---|
| Canonical operations | 139,812 | one normalized state-changing V1 operation | [canonical-operations.jsonl]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/data/slot-440459364-06c0e05aadb7e883/canonical-operations.jsonl)<br>[canonical-operations.parquet]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/data/slot-440459364-06c0e05aadb7e883/parquet/canonical-operations.parquet) | Exact; zero normalization gaps |
| Canonical events | 264,017 | one canonical decoded event/assertion | [canonical-events.jsonl]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/data/slot-440459364-06c0e05aadb7e883/canonical-events.jsonl)<br>[canonical-events.parquet]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/data/slot-440459364-06c0e05aadb7e883/parquet/canonical-events.parquet) | Exact chronological order |
| V1 replay ledger | 139,825 | one factual replay transition | [v1-replay-ledger.jsonl]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/results/runs/slot-440459364-06c0e05aadb7e883/v1-replay-ledger.jsonl)<br>[v1-replay-ledger.parquet]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/data/slot-440459364-06c0e05aadb7e883/parquet/v1-replay-ledger.parquet) | Zero raw-unit differences; zero conservation failures |
| Historical prices | 22,071 | one mint-hour historical USD mark | [historical-prices.jsonl]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/data/slot-440459364-06c0e05aadb7e883/historical-prices.jsonl)<br>[historical-prices.parquet]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/data/slot-440459364-06c0e05aadb7e883/parquet/historical-prices.parquet) | Historical-only; no interpolation/current fallback |
| Decoded transactions | 137,280 | one successful program transaction | [decoded-transactions.jsonl]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/data/slot-440459364-06c0e05aadb7e883/decoded-transactions.jsonl) | Zero transaction decode failures |
| Decoded instructions | 462,312 | one decoded program instruction | [decoded-instructions.jsonl]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/data/slot-440459364-06c0e05aadb7e883/decoded-instructions.jsonl) | 160,749/160,749 state-changing instructions decoded |
| Execution provenance | 137,280 | one transaction execution/provenance record | [execution-provenance.jsonl]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/data/slot-440459364-06c0e05aadb7e883/execution-provenance.jsonl) | Exact instruction/CPI ownership evidence |
| Database events | 895,276 | one indexer event row | [db-events.jsonl]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/data/slot-440459364-06c0e05aadb7e883/db-events.jsonl) | Discovery source; reconciled against RPC |
| DB/RPC reconciliation | 263,977 | one database-to-RPC event comparison | [db-rpc-reconciliation.jsonl]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/results/runs/slot-440459364-06c0e05aadb7e883/db-rpc-reconciliation.jsonl) | 263,977/263,977 pass or documented duplicate/omission |
| Archived proof blocks | 115,850 | one proof-bound finalized block per target slot | [blocks]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/data/slot-440459364-06c0e05aadb7e883/blocks) | 115,850/115,850 present; zero unresolved blocks |

## Five-row samples

- [canonical-operations]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/results/final-report/data-samples/canonical-operations.sample.json)
- [v1-replay-ledger]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/results/final-report/data-samples/v1-replay-ledger.sample.json)
- [historical-prices]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/results/final-report/data-samples/historical-prices.sample.json)
- [db-rpc-reconciliation]($DUSK_REPO_ROOT/analysis/v1-vs-dusk/results/final-report/data-samples/db-rpc-reconciliation.sample.json)

## What is missing

- No final calibrated Dusk winner dataset exists: calibration stopped with zero complete scores.
- The full V1 row-level history, provenance, prices, reconciliation, and replay ledger are present above.
