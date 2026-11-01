#!/usr/bin/env python3
"""Replace the invalid scoped benchmark with a fail-closed correction report."""

from __future__ import annotations

import hashlib
import json
import sqlite3
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
REPORT_DIR = ROOT / "results" / "scoped-three-market" / "report"
EVIDENCE_DIR = REPORT_DIR / "evidence"
CACHE_DIR = ROOT / "results" / "scoped-three-market" / "cache"
RUN_DIR = ROOT / "results" / "runs" / "slot-440459364-06c0e05aadb7e883"


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_json(path: Path, value) -> None:
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def build_notebook(path: Path) -> None:
    notebook = {
        "cells": [
            {
                "cell_type": "markdown",
                "metadata": {},
                "source": [
                    "# Dusk vs V1 benchmark correction\n",
                    "\n",
                    "The previous three-market venue verdict is retracted. This notebook verifies the evidence boundary; it does not calculate a replacement winner.\n",
                ],
            },
            {
                "cell_type": "code",
                "execution_count": None,
                "metadata": {},
                "outputs": [],
                "source": [
                    "import json\n",
                    "from pathlib import Path\n",
                    "evidence = json.loads(Path('evidence/benchmark-correction.json').read_text())\n",
                    "assert evidence['status'] == 'retracted_invalid_comparison'\n",
                    "assert evidence['valid_evidence']['v1_raw_unit_differences'] == 0\n",
                    "assert evidence['valid_evidence']['v1_conservation_failures'] == 0\n",
                    "assert evidence['invalid_dusk_candidate']['complete_for_scoring'] is False\n",
                    "assert evidence['invalid_dusk_candidate']['candidate_score'] is None\n",
                    "assert evidence['invalid_dusk_candidate']['combined_score'] is None\n",
                    "assert all(row['verdict'] == 'No valid verdict' for row in evidence['questions'])\n",
                    "print('Correction verified: no venue winner is supportable.')\n",
                ],
            },
            {
                "cell_type": "markdown",
                "metadata": {},
                "source": [
                    "## What remains valid\n",
                    "\n",
                    "The historical V1 reconstruction and its conservation checks remain usable. The sampled Dusk replay remains diagnostic evidence only.\n",
                ],
            },
        ],
        "metadata": {
            "kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
            "language_info": {"name": "python", "version": "3"},
        },
        "nbformat": 4,
        "nbformat_minor": 5,
    }
    write_json(path, notebook)


def main() -> None:
    EVIDENCE_DIR.mkdir(parents=True, exist_ok=True)
    arms = {read_json(path)["arm"]: (path, read_json(path)) for path in CACHE_DIR.glob("*.arm.json")}
    fixed_path, fixed = arms["fixed_flow"]
    intent_path, intent = arms["intent"]
    profile_path = next(CACHE_DIR.glob("*.profile.json"))
    combined_profile = read_json(profile_path)
    score = fixed["score_evidence"]
    reconciliation_path = RUN_DIR / "v1-reconciliation.json"
    reconciliation = read_json(reconciliation_path)
    generated_at = datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")

    questions = [
        {
            "question": "Collateral efficiency",
            "verdict": "No valid verdict",
            "why": "The published values were sums across repeated borrow observations, not debt-to-collateral efficiency ratios; the Dusk candidate was incomplete and unscored.",
            "required_next_evidence": "A complete named or frozen profile plus paired debt/collateral, filled-demand, and unmet-demand metrics at the same checkpoints.",
        },
        {
            "question": "Spot capital efficiency",
            "verdict": "No valid verdict",
            "why": "The sampled Dusk candidate produced no comparable execution-cost or slippage observations, so its near-zero revenue is mostly rejected historical flow, not venue economics.",
            "required_next_evidence": "Paired executed swaps and standardized probes with output, cost, slippage, equivalent depth, fee return, and capital utilization.",
        },
        {
            "question": "Leverage",
            "verdict": "No valid verdict",
            "why": "No frozen leverage campaign was executed; projected debt observation-sums are not leverage or maintenance-margin results.",
            "required_next_evidence": "The exact leverage grid with opening/unwind costs, executable leverage, margin buffers, funding, adverse paths, and liquidation outcomes.",
        },
        {
            "question": "LP and protocol economics",
            "verdict": "No valid verdict",
            "why": "The Dusk replay deferred 77 external-state operations and was incomplete, so raw revenue totals are not comparable venue revenue or yield.",
            "required_next_evidence": "A complete replay with capital-time denominators and all revenue lanes separated by origin and accounting class.",
        },
    ]
    blocker_counts = [
        {"blocker": "Score blocking reasons", "count": len(score["blocking_reasons"])},
        {"blocker": "Incomplete replay flag", "count": int(not fixed["counterfactual"]["complete_for_scoring"])},
        {"blocker": "Null arm candidate score", "count": int(score["candidate_score"] is None)},
        {"blocker": "Null combined profile score", "count": int(combined_profile["combined_score"] is None)},
        {"blocker": "Mixed five-/three-market grain", "count": 1},
    ]
    question_values = ",\n  ".join(
        f"({index}, {sql_literal(row['question'])}, {sql_literal(row['verdict'])}, {sql_literal(row['why'])})"
        for index, row in enumerate(questions, start=1)
    )
    blocker_values = ",\n  ".join(
        f"({sql_literal(row['blocker'])}, {row['count']})" for row in blocker_counts
    )
    questions_sql = f"""WITH benchmark_questions(display_order, question, verdict, why) AS (
  VALUES
  {question_values}
)
SELECT question, verdict, why
FROM benchmark_questions
ORDER BY display_order"""
    blockers_sql = f"""WITH publication_blockers(blocker, count) AS (
  VALUES
  {blocker_values}
)
SELECT blocker, count
FROM publication_blockers
ORDER BY count DESC, blocker ASC"""
    sql_path = EVIDENCE_DIR / "benchmark-correction.sql"
    sql_path.write_text(
        "-- Exact report projections generated from benchmark-correction.json inputs.\n"
        + questions_sql
        + ";\n\n"
        + blockers_sql
        + ";\n",
        encoding="utf-8",
    )
    with sqlite3.connect(":memory:") as connection:
        projected_questions = [
            {"question": row[0], "verdict": row[1], "why": row[2]}
            for row in connection.execute(questions_sql)
        ]
        projected_blockers = [
            {"blocker": row[0], "count": row[1]}
            for row in connection.execute(blockers_sql)
        ]
    if projected_questions != [
        {"question": row["question"], "verdict": row["verdict"], "why": row["why"]}
        for row in questions
    ]:
        raise RuntimeError("question SQL projection differs from correction evidence")
    if sorted(projected_blockers, key=lambda row: row["blocker"]) != sorted(
        blocker_counts, key=lambda row: row["blocker"]
    ):
        raise RuntimeError("blocker SQL projection differs from correction evidence")

    evidence = {
        "schema_version": 2,
        "generated_at": generated_at,
        "status": "retracted_invalid_comparison",
        "decision": "No V1-versus-Dusk winner is supportable from the current scoped artifacts.",
        "valid_evidence": {
            "run_id": "slot-440459364-06c0e05aadb7e883",
            "v1_canonical_operations": reconciliation["canonical_operation_count"],
            "v1_replayed_transitions": reconciliation["replayed_transition_count"],
            "v1_markets": reconciliation["market_count"],
            "v1_raw_unit_differences": reconciliation["nonzero_raw_unit_difference_count"],
            "v1_conservation_failures": reconciliation["conservation_failure_count"],
            "scope_note": "These facts validate the reconstructed V1 history; they do not validate a Dusk comparison.",
        },
        "invalid_dusk_candidate": {
            "profile_id": fixed["profile_id"],
            "selection_basis": "Only sampled profile among 32 diagnostic candidates that initialized the selected historical markets; not program defaults and not a calibrated/frozen winner.",
            "complete_for_scoring": fixed["counterfactual"]["complete_for_scoring"],
            "executed_count": fixed["counterfactual"]["executed_count"],
            "partially_filled_count": fixed["counterfactual"]["partially_filled_count"],
            "rejected_count": fixed["counterfactual"]["rejected_count"],
            "deferred_external_state_count": fixed["counterfactual"]["deferred_external_state_count"],
            "candidate_score": score["candidate_score"],
            "combined_score": combined_profile["combined_score"],
            "blocking_reasons": score["blocking_reasons"],
            "grain_warning": "Replay status counts cover the five-market input history while the published projection filtered score rows to three markets.",
        },
        "program_defaults_baseline": {
            "status": "failed_before_comparable_metrics",
            "error_name": "DebtShareDivisionOverflow",
            "error_code": 6063,
            "source": "programs/dusk/src/state/market.rs:1309",
            "elapsed_seconds": 41.47,
            "interpretation": "The checked-in defaults are a prospective code baseline, not a deployed configuration. They cannot currently replay this retained history to completion.",
        },
        "questions": questions,
        "publication_gate": {
            "requirements": [
                "Use a named Dusk baseline or a training-calibrated profile frozen before holdout.",
                "Replay exactly the three selected markets plus required global state without mixing five-market status counts.",
                "Require complete_for_scoring=true, non-null candidate and combined scores, and zero blocking reasons.",
                "Publish only the four preregistered metrics with their correct denominators; otherwise mark the metric unavailable.",
            ]
        },
        "sources": {
            "fixed_flow_arm": {"sha256": sha256_file(fixed_path), "bytes": fixed_path.stat().st_size},
            "intent_arm": {"sha256": sha256_file(intent_path), "bytes": intent_path.stat().st_size},
            "combined_profile": {"sha256": sha256_file(profile_path), "bytes": profile_path.stat().st_size},
            "v1_reconciliation": {"sha256": sha256_file(reconciliation_path), "bytes": reconciliation_path.stat().st_size},
        },
    }
    evidence_path = EVIDENCE_DIR / "benchmark-correction.json"
    write_json(evidence_path, evidence)

    sources = [
        {
            "id": "correction",
            "label": "Benchmark correction evidence",
            "path": "evidence/benchmark-correction.sql",
            "description": "Fail-closed audit generated by scripts/build_scoped_benchmark_correction.py from the retained fixed-flow arm, intent arm, combined profile, and V1 reconciliation artifacts.",
        }
    ]
    artifact = {
        "surface": "report",
        "manifest": {
            "version": 1,
            "surface": "report",
            "title": "Dusk vs V1: Benchmark Correction",
            "description": "Retraction of the invalid three-market venue verdict and the exact evidence needed for a valid comparison.",
            "generatedAt": generated_at,
            "cards": [],
            "charts": [
                {
                    "id": "blockers",
                    "title": "Publication blockers in the prior Dusk comparison",
                    "subtitle": "Exact artifact conditions; counts indicate occurrences, not severity",
                    "intent": "comparison",
                    "question": "Why can the prior Dusk candidate not support a venue verdict?",
                    "rationale": "A horizontal bar chart makes the seven explicit score blockers and four binary publication failures immediately visible.",
                    "type": "horizontalBar",
                    "dataset": "blocker_counts",
                    "sourceId": "correction",
                    "encodings": {
                        "x": {"field": "blocker", "type": "nominal", "label": "Blocking condition"},
                        "y": {"field": "count", "type": "quantitative", "format": "number", "label": "Count"},
                    },
                    "valueFormat": "number",
                    "layout": "full",
                    "labels": {"values": "all"},
                    "palette": {"kind": "categorical", "name": "tableau10"},
                    "settings": {"orientation": "horizontal", "sort": "descending", "showValues": True},
                }
            ],
            "tables": [],
            "sources": sources,
            "blocks": [
                {"id": "title", "type": "markdown", "layout": "full", "body": "# Dusk vs V1: Benchmark Correction"},
                {
                    "id": "summary",
                    "type": "markdown",
                    "layout": "full",
                    "sourceId": "correction",
                    "body": "## Executive Summary\n\n- **The previous `V1 wins` report is retracted.** Its Dusk side was one uncalibrated random-search candidate, not the program defaults or a frozen winner.\n- **No venue winner is currently supportable.** The candidate replay was incomplete and its candidate and combined scores were null.\n- **The V1 history remains valid.** Exact reconciliation passed with zero raw-unit differences and zero conservation failures, but that proves only the factual control.\n- **The checked-in Dusk defaults also do not supply a comparison.** They stop during retained-history replay with a debt-share division overflow before comparable metrics exist."
                },
                {
                    "id": "questions_intro",
                    "type": "markdown",
                    "layout": "full",
                    "body": "## The four questions are still unanswered\n\n- **Collateral efficiency - No valid verdict.** The published values were repeated borrow-observation sums, not debt-to-collateral efficiency ratios.\n- **Spot capital efficiency - No valid verdict.** There were no comparable Dusk execution-cost or slippage observations.\n- **Leverage - No valid verdict.** No frozen leverage campaign was executed; projected debt sums are not leverage results.\n- **LP and protocol economics - No valid verdict.** The incomplete Dusk replay deferred 77 external-state operations, so its revenue is not comparable venue revenue or yield."
                },
                {
                    "id": "failure",
                    "type": "markdown",
                    "layout": "full",
                    "sourceId": "correction",
                    "body": f"## Why the old result fails\n\n**The sampled candidate is diagnostic only.** It executed {fixed['counterfactual']['executed_count']:,} operations, partially filled {fixed['counterfactual']['partially_filled_count']:,}, rejected {fixed['counterfactual']['rejected_count']:,}, and deferred {fixed['counterfactual']['deferred_external_state_count']} external-state operations. Its replay explicitly records `complete_for_scoring=false`; both score fields are null. Near-zero Dusk revenue therefore measures rejected historical flow under that candidate, not Dusk's venue economics.\n\n**The metric names also changed their meaning.** Repeated borrow-state observation sums were presented as collateral efficiency and leverage, even though neither a debt-to-collateral ratio nor a leverage campaign was computed. Five-market replay-status counts were also mixed with a three-market score projection."
                },
                {
                    "id": "blocker_explanation",
                    "type": "markdown",
                    "layout": "full",
                    "sourceId": "correction",
                    "body": "**The comparison fails multiple independent publication gates.** The seven score blockers are explicit reasons emitted by the scoring engine; the remaining bars are binary artifact failures. These counts are not performance scores and should not be compared as magnitudes."
                },
                {"id": "blocker_chart", "type": "chart", "layout": "full", "chartId": "blockers"},
                {
                    "id": "valid",
                    "type": "markdown",
                    "layout": "full",
                    "sourceId": "correction",
                    "body": f"## What data remains trustworthy\n\nThe retained V1 reconstruction covers **{reconciliation['canonical_operation_count']:,} canonical operations**, **{reconciliation['replayed_transition_count']:,} replay transitions**, and **{reconciliation['market_count']} selected markets** with **zero raw-unit differences** and **zero conservation failures**. That is valuable factual history, but it cannot answer the counterfactual questions until a valid Dusk profile completes the same scope."
                },
                {
                    "id": "next",
                    "type": "markdown",
                    "layout": "full",
                    "body": "## Required next steps\n\n1. Define a named prospective Dusk baseline or calibrate on training data and freeze it before holdout.\n2. Replay exactly the three selected markets plus required global state.\n3. Require a complete, blocker-free, non-null score before opening validation or reporting comparisons.\n4. Calculate the four original metric families with their real denominators; label unsupported metrics `Unavailable`."
                },
                {
                    "id": "further",
                    "type": "markdown",
                    "layout": "full",
                    "body": "## Further Questions\n\n- Which prospective Dusk configuration is the intended product baseline? The checked-in defaults are full-range safety defaults and currently overflow on this replay.\n- Should the final comparison show both a named design baseline and a separately calibrated opportunity frontier? Keeping them separate avoids presenting optimization as a default product claim."
                },
                {
                    "id": "caveats",
                    "type": "markdown",
                    "layout": "full",
                    "body": "## Caveats and Assumptions\n\nThis is a correction report, not a replacement performance benchmark. Dusk is under development and has not been deployed; the phrase `program defaults` refers only to checked-in prospective code. No prior V1-wins number should be cited."
                },
            ],
        },
        "snapshot": {
            "version": 1,
            "generatedAt": generated_at,
            "status": "ready",
            "datasets": {"questions": questions, "blocker_counts": blocker_counts},
        },
        "sources": sources,
    }
    write_json(REPORT_DIR / "artifact.json", artifact)
    build_notebook(REPORT_DIR / "scoped-benchmark.ipynb")


if __name__ == "__main__":
    main()
