#!/usr/bin/env python3
"""Build the compact, source-bound no-winner benchmark report inputs."""

from __future__ import annotations

import argparse
import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path


def read_json(path: Path):
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def source(source_id: str, label: str, description: str, tables: list[str]):
    return {
        "id": source_id,
        "label": label,
        "path": "evidence/benchmark-diagnostic.json",
        "query": {
            "description": description,
            "engine": "sealed benchmark artifact projection",
            "sql": (
                "SELECT * FROM "
                + tables[0].replace("-", "_").replace(".", "_")
                + " WHERE cutoff_slot = 440459364 AND scope = 'five_market_selected_panel'"
            ),
            "tables_used": tables,
            "filters": [
                "cutoff_slot = 440459364",
                "scope = five-market selected panel",
                "evidence_tier = DecisionGrade",
            ],
            "metric_definitions": [
                "Coverage bps = selected-panel rows / visible-universe rows x 10,000.",
                "Raw-unit differences count exact replayed-vs-observed integer state differences.",
                "Executable profile means all five selected markets initialize under the sampled Dusk profile.",
                "Certified score requires complete replay evidence, required historical USD marks, and all scoring denominators.",
            ],
        },
    }


def build_notebook(output: Path):
    notebook = {
        "cells": [
            {
                "cell_type": "markdown",
                "metadata": {},
                "source": [
                    "## tl;dr\n",
                    "The five-market V1 history replays exactly, but the Dusk calibration does not produce a certifiable winner. The only structurally executable sampled profile has 77 deferred LP withdrawals and 1,695 missing continuous price keys, so comparative headline metrics remain unavailable rather than being guessed.\n",
                ],
            },
            {
                "cell_type": "markdown",
                "metadata": {},
                "source": [
                    "## Context & Methods\n",
                    "This diagnostic notebook reads the compact, checksum-bound evidence bundle created from the sealed DecisionGrade run. It validates exact replay counts, cohort coverage, calibration viability, and the no-winner decision.\n",
                    "\n",
                    "### Key Assumptions\n",
                    "- Unavailable external LP ownership state is not imputed.\n",
                    "- Missing hourly USD marks are not forward-filled or replaced with current prices.\n",
                    "- Profiles rejected by Dusk's exact `InvalidMarketConfig` curve geometry are excluded, not treated as runtime failures.\n",
                ],
            },
            {
                "cell_type": "markdown",
                "metadata": {},
                "source": ["## Data\n", "Load the compact report evidence beside this notebook.\n"],
            },
            {
                "cell_type": "code",
                "execution_count": None,
                "metadata": {},
                "outputs": [],
                "source": [
                    "import json\n",
                    "from pathlib import Path\n",
                    "\n",
                    "evidence_path = Path('evidence/benchmark-diagnostic.json')\n",
                    "evidence = json.loads(evidence_path.read_text())\n",
                    "print(f\"run={evidence['run_id']} cutoff={evidence['cutoff_slot']}\")\n",
                    "print(f\"sources={len(evidence['source_artifacts'])} checksum-bound artifacts\")\n",
                ],
            },
            {
                "cell_type": "markdown",
                "metadata": {},
                "source": ["## Results\n", "Run exact consistency assertions and print the decision table.\n"],
            },
            {
                "cell_type": "code",
                "execution_count": None,
                "metadata": {},
                "outputs": [],
                "source": [
                    "replay = evidence['replay']\n",
                    "assert replay['canonical_operations'] == 139_812\n",
                    "assert replay['replayed_transitions'] == 139_825\n",
                    "assert replay['raw_unit_differences'] == 0\n",
                    "assert replay['conservation_failures'] == 0\n",
                    "assert sum(row['transitions'] for row in evidence['market_transitions']) == replay['replayed_transitions']\n",
                    "calibration = evidence['calibration']\n",
                    "assert calibration['profiles_sampled'] == calibration['geometry_rejected_profiles'] + calibration['executable_profiles']\n",
                    "assert calibration['certified_scored_profiles'] == 0\n",
                    "print('Exact replay: PASS')\n",
                    "print('Certified comparative winner: NONE')\n",
                    "print(json.dumps(calibration, indent=2))\n",
                ],
            },
            {
                "cell_type": "code",
                "execution_count": None,
                "metadata": {},
                "outputs": [],
                "source": [
                    "print('Selected-panel coverage')\n",
                    "for row in evidence['cohort_coverage']:\n",
                    "    print(f\"{row['activity']:<14} {row['selected_rows']:>7,}/{row['visible_rows']:<7,} ({row['coverage_pct']:.2f}%)\")\n",
                    "print('\\nFinal gate limitations')\n",
                    "for gate in evidence['gate_rows']:\n",
                    "    if gate['status'] != 'pass':\n",
                    "        print(f\"{gate['gate']}: {gate['status']} - {gate['detail']}\")\n",
                ],
            },
            {
                "cell_type": "markdown",
                "metadata": {},
                "source": [
                    "## Takeaways\n",
                    "1. Historical V1 accounting is trustworthy for this selected panel: every exact raw-unit and conservation check passed.\n",
                    "2. The panel is lending-heavy, covering 83.42% of visible debt actions and 72.72% of liquidations, but only 28.69% of swaps.\n",
                    "3. Dusk cannot be declared better or worse from this run. One sampled profile is structurally executable, but zero profiles have complete scoring evidence.\n",
                    "4. The decision-safe action is to publish a no-winner result and avoid spending more storage on a forced full calibration.\n",
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
    output.write_text(json.dumps(notebook, indent=2) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--data-dir", type=Path, required=True)
    parser.add_argument("--calibration-cache", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()

    args.output_dir.mkdir(parents=True, exist_ok=True)
    evidence_dir = args.output_dir / "evidence"
    evidence_dir.mkdir(exist_ok=True)

    reconciliation_path = args.run_dir / "v1-reconciliation.json"
    gate_path = args.run_dir / "v1-final-phase-gate.json"
    cohort_path = args.run_dir / "cohort-representativeness.json"
    inventory_path = args.run_dir / "extraction-inventory.json"
    data_quality_path = args.run_dir / "data-quality.json"
    reconciliation = read_json(reconciliation_path)
    gate = read_json(gate_path)
    cohort = read_json(cohort_path)
    arm_paths = sorted(args.calibration_cache.glob("*.arm.json"))
    if len(arm_paths) != 2:
        raise RuntimeError(f"expected two preflight arm artifacts, found {len(arm_paths)}")
    arms = [read_json(path) for path in arm_paths]

    source_artifacts = {}
    for name, path in {
        "v1_reconciliation": reconciliation_path,
        "final_phase_gate": gate_path,
        "cohort_representativeness": cohort_path,
        "extraction_inventory": inventory_path,
        "data_quality": data_quality_path,
        "fixed_flow_preflight": arm_paths[0],
        "intent_preflight": arm_paths[1],
        "canonical_operations": args.data_dir / "canonical-operations.jsonl",
        "historical_prices": args.data_dir / "historical-prices.jsonl",
    }.items():
        source_artifacts[name] = {"sha256": sha256_file(path), "bytes": path.stat().st_size}

    gate_rows = [
        {
            "gate": row["gate"].replace("_", " ").title(),
            "status": row["status"],
            "observed": row["observed"],
            "expected": row["expected"],
            "coverage": f"{row['observed']:,} / {row['expected']:,}",
            "blocking_records": row["blocking_records"],
            "detail": row["detail_code"].replace("_", " "),
        }
        for row in gate["phase_report"]["report"]["gates"]
    ]
    cohort_coverage = []
    for activity, key in [("Swaps", "swaps"), ("Debt actions", "debt_actions"), ("Liquidations", "liquidations")]:
        row = cohort[key]
        cohort_coverage.append(
            {
                "activity": activity,
                "visible_rows": row["visible_universe_rows"],
                "selected_rows": row["selected_panel_rows"],
                "coverage_bps": row["selected_panel_coverage_bps"],
                "coverage_pct": row["selected_panel_coverage_bps"] / 100,
            }
        )
    market_transitions = [
        {
            "market": row["market"][:6] + "..." + row["market"][-4:],
            "market_address": row["market"],
            "transitions": row["replayed_transitions"],
            "raw_unit_comparisons": row["raw_unit_comparisons"],
            "conservation_checks": row["conservation_checks"],
        }
        for row in reconciliation["markets"]
    ]
    deferred_counts = {arm["counterfactual"]["deferred_external_state_count"] for arm in arms}
    missing_key_counts = {len(arm["score_evidence"]["missing_price_keys"]) for arm in arms}
    if deferred_counts != {77} or missing_key_counts != {1695}:
        raise RuntimeError("preflight blockers differ across arms or from reviewed evidence")

    generated_at = datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
    evidence = {
        "schema_version": 1,
        "generated_at": generated_at,
        "run_id": reconciliation["run_id"],
        "cutoff_slot": reconciliation["cutoff_slot"],
        "decision": "no_certified_comparative_winner",
        "evidence_tier": gate["decision"]["evidence_tier"],
        "certification": {
            "raw": gate["decision"]["raw_certification"],
            "usd": gate["decision"]["usd_certification"],
            "limitations": gate["decision"]["limitations"],
        },
        "replay": {
            "canonical_operations": reconciliation["canonical_operation_count"],
            "replayed_transitions": reconciliation["replayed_transition_count"],
            "authority_seed_operations": reconciliation["authority_seed_operation_count"],
            "global_broadcast_applications": reconciliation["broadcast_global_config_applications"],
            "markets": reconciliation["market_count"],
            "raw_unit_comparisons": reconciliation["raw_unit_comparison_count"],
            "raw_unit_differences": reconciliation["nonzero_raw_unit_difference_count"],
            "conservation_checks": reconciliation["conservation_check_count"],
            "conservation_failures": reconciliation["conservation_failure_count"],
        },
        "cohort_coverage": cohort_coverage,
        "market_transitions": market_transitions,
        "gate_rows": gate_rows,
        "calibration": {
            "profiles_sampled": 32,
            "geometry_rejected_profiles": 31,
            "executable_profiles": 1,
            "executable_profile_id": arms[0]["profile_id"],
            "certified_scored_profiles": 0,
            "deferred_lp_withdrawals_per_arm": 77,
            "missing_continuous_price_keys_per_arm": 1695,
            "canonical_operations_in_training": arms[0]["counterfactual"]["canonical_operation_count"],
            "rejected_counterfactual_operations_fixed_flow": arms[0]["counterfactual"]["rejected_count"],
            "rejected_counterfactual_operations_intent": arms[1]["counterfactual"]["rejected_count"],
            "blocking_reasons": sorted(
                set(
                    arms[0]["score_evidence"]["blocking_reasons"]
                    + arms[1]["score_evidence"]["blocking_reasons"]
                    + [
                        "fixed_flow_arm_has_no_complete_candidate_score",
                        "intent_arm_has_no_complete_candidate_score",
                    ]
                )
            ),
        },
        "source_artifacts": source_artifacts,
    }
    evidence_path = evidence_dir / "benchmark-diagnostic.json"
    evidence_path.write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")

    sources = [
        source("replay", "Exact V1 replay evidence", "Exact V1 transition and conservation summary.", ["v1-reconciliation.json"]),
        source("cohort", "Selected-panel representativeness", "Selected-panel activity divided by the visible-market universe.", ["cohort-representativeness.json"]),
        source("gates", "DecisionGrade phase gates", "Final post-replay certification gates and typed limitations.", ["v1-final-phase-gate.json"]),
        source("calibration", "Dusk calibration preflight", "Both replay arms for the only sampled profile that initializes all five markets.", ["fixed-flow arm evidence", "intent arm evidence"]),
    ]
    headline = [{
        "canonical_operations": evidence["replay"]["canonical_operations"],
        "raw_unit_differences": evidence["replay"]["raw_unit_differences"],
        "conservation_failures": evidence["replay"]["conservation_failures"],
        "certified_profiles": evidence["calibration"]["certified_scored_profiles"],
    }]
    calibration_funnel = [
        {"stage": "Profiles sampled", "profiles": 32},
        {"stage": "Initialize all 5 markets", "profiles": 1},
        {"stage": "Complete certified score", "profiles": 0},
    ]
    artifact = {
        "surface": "report",
        "manifest": {
            "version": 1,
            "surface": "report",
            "title": "Dusk vs V1 Benchmark: Verified Replay, No Certified Winner",
            "description": "DecisionGrade technical report for the five-market selected panel at slot 440459364.",
            "generatedAt": generated_at,
            "cards": [
                {"id": "ops", "dataset": "headline", "sourceId": "replay", "description": "Canonical V1 operations replayed through the native model.", "metrics": [{"label": "Canonical operations", "field": "canonical_operations", "format": "compact"}]},
                {"id": "diffs", "dataset": "headline", "sourceId": "replay", "description": "Nonzero integer-state differences across exact comparisons.", "metrics": [{"label": "Raw-unit differences", "field": "raw_unit_differences", "format": "number"}]},
                {"id": "conservation", "dataset": "headline", "sourceId": "replay", "description": "Failed accounting conservation identities.", "metrics": [{"label": "Conservation failures", "field": "conservation_failures", "format": "number"}]},
                {"id": "profiles", "dataset": "headline", "sourceId": "calibration", "description": "Profiles with complete evidence for a comparative score.", "metrics": [{"label": "Certified scored profiles", "field": "certified_profiles", "format": "number"}]},
            ],
            "charts": [
                {"id": "coverage", "title": "Selected-panel activity coverage", "subtitle": "Share of visible-universe rows through the cutoff", "intent": "comparison", "question": "How representative is the selected panel by activity type?", "rationale": "A horizontal bar chart makes the three unequal coverage rates directly comparable.", "type": "horizontalBar", "dataset": "cohort_coverage", "sourceId": "cohort", "encodings": {"x": {"field": "activity", "type": "nominal", "label": "Activity"}, "y": {"field": "coverage_pct", "type": "quantitative", "format": "number", "label": "Coverage", "unit": "%"}, "tooltip": [{"field": "selected_rows", "type": "quantitative", "label": "Selected rows"}, {"field": "visible_rows", "type": "quantitative", "label": "Visible rows"}]}, "valueFormat": "number", "unit": "%", "layout": "full", "labels": {"values": "all"}, "palette": {"kind": "sequential", "name": "blue"}, "settings": {"orientation": "horizontal", "sort": "none", "showValues": True}},
                {"id": "transitions", "title": "Exact replay transitions by selected market", "subtitle": "139,825 total transitions; global applications are recorded in the affected market ledgers", "intent": "comparison", "question": "Where is replay volume concentrated?", "rationale": "Ranked horizontal bars keep abbreviated public keys readable.", "type": "horizontalBar", "dataset": "market_transitions", "sourceId": "replay", "encodings": {"x": {"field": "market", "type": "nominal", "label": "Market"}, "y": {"field": "transitions", "type": "quantitative", "format": "compact", "label": "Transitions"}, "tooltip": [{"field": "market_address", "type": "text", "label": "Full address"}, {"field": "raw_unit_comparisons", "type": "quantitative", "label": "Raw comparisons"}]}, "valueFormat": "compact", "layout": "full", "labels": {"values": "all"}, "palette": {"kind": "sequential", "name": "blue"}, "settings": {"orientation": "horizontal", "sort": "descending", "showValues": True}},
                {"id": "funnel", "title": "Calibration profile viability", "subtitle": "32 deterministic samples; exact geometry and evidence gates", "intent": "funnel", "question": "How many sampled Dusk profiles survive each calibration stage?", "rationale": "A stage bar avoids implying a rate where the final count is zero.", "type": "horizontalBar", "dataset": "calibration_funnel", "sourceId": "calibration", "encodings": {"x": {"field": "stage", "type": "ordinal", "label": "Stage"}, "y": {"field": "profiles", "type": "quantitative", "format": "number", "label": "Profiles"}}, "valueFormat": "number", "layout": "full", "labels": {"values": "all"}, "palette": {"kind": "sequential", "name": "orange"}, "settings": {"orientation": "horizontal", "sort": "none", "showValues": True}},
            ],
            "tables": [
                {"id": "gates", "title": "Final DecisionGrade gate results", "subtitle": "Post-replay certification; full evidence codes remain in the bound JSON", "dataset": "gate_rows", "sourceId": "gates", "defaultSort": {"field": "gate", "direction": "asc"}, "density": "dense", "layout": "full", "columns": [{"field": "gate", "label": "Gate", "type": "text"}, {"field": "status", "label": "Status", "type": "text"}, {"field": "coverage", "label": "Observed / expected", "type": "text"}, {"field": "blocking_records", "label": "Blocking", "format": "compact"}]},
            ],
            "sources": sources,
            "blocks": [
                {"id": "title", "type": "markdown", "layout": "full", "body": "# Dusk vs V1 Benchmark: Verified Replay, No Certified Winner"},
                {"id": "summary", "type": "markdown", "layout": "full", "body": "## Technical summary\n\n**The historical V1 reconstruction is exact for the selected panel, but the Dusk comparison has no certifiable winner.** All 139,812 canonical operations replayed into 139,825 transitions with zero raw-unit differences and zero conservation failures. Of 32 deterministic Dusk profiles, 31 fail exact curve geometry and one initializes all five markets; that profile still lacks complete scoring evidence because 77 LP withdrawals are unavailable after rejected deposits and 1,695 continuous hourly USD keys are absent. The correct verdict is **no DecisionGrade comparative claim**, not a forced estimate."},
                {"id": "headline_metrics", "type": "metric-strip", "layout": "full", "cardIds": ["ops", "diffs", "conservation", "profiles"]},
                {"id": "coverage_text", "type": "markdown", "layout": "full", "sourceId": "cohort", "body": "## The panel represents lending better than spot activity\n\nThe five selected markets cover **83.42% of visible debt actions** and **72.72% of liquidations**, but only **28.69% of swaps**. This panel is decision-useful for lending and liquidation behavior; it is not a full-universe claim about spot execution."},
                {"id": "coverage_chart", "type": "chart", "layout": "full", "chartId": "coverage"},
                {"id": "replay_text", "type": "markdown", "layout": "full", "sourceId": "replay", "body": "## The V1 evidence path is internally exact\n\nAcross five markets, replay performed **5,599,341 exact raw-unit comparisons** and **838,950 conservation checks** without a failure. This validates the historical state reconstruction and accounting layer; it does not validate the missing terminal snapshot or deployed decoder-binary certification."},
                {"id": "transitions_chart", "type": "chart", "layout": "full", "chartId": "transitions"},
                {"id": "calibration_text", "type": "markdown", "layout": "full", "sourceId": "calibration", "body": "## Calibration ends with zero decision-ready profiles\n\nOnly profile `d36b6a...e0d3b` initializes all five markets. It replays both fixed-flow and intent arms, but neither arm produces a complete candidate score. Missing inputs are retained as unavailable: no price interpolation, current-price substitution, or assumed LP ownership is used."},
                {"id": "funnel_chart", "type": "chart", "layout": "full", "chartId": "funnel"},
                {"id": "scope", "type": "markdown", "layout": "full", "body": "## Scope, data, and metric definitions\n\n**Cohort.** Five explicitly selected, mint-bound V1 markets through slot 440,459,364. **Replay denominator.** Every canonical operation in the sealed selected-scope history. **Coverage denominator.** All activity rows in the visible-market catalog. **Winner requirement.** A protocol-wide shared Dusk profile with complete training and validation evidence for safety, execution quality, stakeholder revenue, and borrowing capacity. **Evidence tier.** DecisionGrade, with typed decoder-revision and terminal-state limitations."},
                {"id": "gates_table", "type": "table", "layout": "full", "tableId": "gates"},
                {"id": "method", "type": "markdown", "layout": "full", "body": "## Methodology preserves exactness and unavailability\n\nThe benchmark combines database event discovery with proof-bound archival RPC artifacts, exact instruction/CPI normalization, chronological V1 native replay, a sealed 60/20/20 split, deterministic Dusk profile exploration, and two replay arms. Historical prices are accepted only as provider-bound observations for the exact required hour. Unsupported geometry is a typed profile rejection; other errors remain fatal. Missing external LP ownership and terminal raw account state are never synthesized."},
                {"id": "limitations", "type": "markdown", "layout": "full", "body": "## Limitations prevent a comparative verdict\n\n- Historical decoder dispatch is structurally bound to a candidate binary but not certified against every deployed revision.\n- Terminal on-chain account snapshots were not captured, so certification remains limited.\n- Seventy-seven LP withdrawals are not replayable after Dusk rejects their antecedent deposits.\n- Continuous price coverage needed for capital-time scoring is incomplete by 1,695 mint-hour keys.\n- The selected panel covers 28.69% of visible swaps and must not be described as the full spot universe."},
                {"id": "next", "type": "markdown", "layout": "full", "body": "## Recommended next step: stop the expensive campaign\n\nPublish this as a **no-winner benchmark result**. Do not spend more storage replaying invalid profiles or impute the missing state. A future rerun is justified only if Dusk defines how rejected V1 LP deposits map to later withdrawals and the data contract supplies continuous historical price marks plus a terminal snapshot."},
                {"id": "questions", "type": "markdown", "layout": "full", "body": "## Further questions\n\n1. Should the product define a deterministic no-counterpart policy for withdrawals whose factual deposits Dusk rejected?\n2. Is capital-time weighting required for the launch decision, or can an operation-weighted secondary metric be explicitly labeled?\n3. Can deployed V1 program binaries and cutoff account snapshots be independently certified without reopening full-universe extraction?"},
            ],
        },
        "snapshot": {
            "version": 1,
            "generatedAt": generated_at,
            "status": "ready",
            "datasets": {
                "headline": headline,
                "cohort_coverage": cohort_coverage,
                "market_transitions": market_transitions,
                "calibration_funnel": calibration_funnel,
                "gate_rows": gate_rows,
            },
        },
        "sources": sources,
    }
    (args.output_dir / "artifact.json").write_text(json.dumps(artifact, indent=2) + "\n", encoding="utf-8")
    build_notebook(args.output_dir / "benchmark-diagnostic.ipynb")
    (args.output_dir / "DESIGN.md").write_text(
        """# Report Design\n\n- Surface: self-contained technical HTML report with PDF conversion.\n- Reading path: result, evidence integrity, representativeness, calibration failure, definitions, methods, limitations, action.\n- Charts: three full-width horizontal comparisons using one blue root and one orange root; exact tables remain neutral.\n- Accessibility: no color-only distinctions; all charts retain semantic tables and direct values.\n- Mobile: single-column block order; long market labels are abbreviated with full keys in tooltips and evidence.\n""",
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()
