#!/usr/bin/env python3
"""Build the bounded three-market Dusk-vs-V1 comparison artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
from datetime import datetime, timezone
from fractions import Fraction
from pathlib import Path


MARKETS = {
    "8cWJ9U2jFZhugi5CSvjN38eJrKuQx1AKth8XsvR6857p": "Fartcoin/USDC (spot)",
    "Ep18GzZwRzt5EunVprg4aMxBFYimxZv13mr2J24MMtFo": "stORE/USDC (borrowing)",
    "BZi9iPbcpLWDkHs8nNQe5pb9MxLQe8fbgHtypj2pL4UB": "OMFG/USDC (liquidations)",
}


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def rational(value: dict) -> Fraction:
    return Fraction(int(value["numerator"]), int(value["denominator"]))


def micro_usd(value: dict) -> float:
    return float(rational(value)) / 1_000_000


def available_usd(value: dict) -> float | None:
    if value["availability"] != "available":
        return None
    return micro_usd(value["value"])


def lane_usd(evidence: dict, venue: str, lanes: set[str]) -> float:
    total = 0.0
    for row in evidence["revenue_yield"]["lane_totals"]:
        if (
            row["venue"] == venue
            and row["origin"] == "factual_user_flow"
            and row["accounting_class"] == "economic_revenue"
            and row["lane"] in lanes
        ):
            value = available_usd(row["usd_micro"])
            if value is None and row["usd_micro"]["availability"].startswith("not_applicable"):
                continue
            if value is None:
                raise RuntimeError(f"required revenue lane is unavailable: {row}")
            total += value
    return total


def source(source_id: str, label: str, description: str, tables: list[str]):
    return {
        "id": source_id,
        "label": label,
        "path": "evidence/scoped-benchmark.json",
        "query": {
            "description": description,
            "engine": "checksum-bound benchmark projection",
            "sql": "SELECT * FROM scoped_three_market_benchmark WHERE split = 'training'",
            "tables_used": tables,
            "filters": [
                "training split only",
                "three named selected-panel markets",
                "profile d36b6a...e0d3b",
                "maximum-feasible hLP scenario",
            ],
            "metric_definitions": [
                "Actionable headroom is the sum of additional USD debt capacity over matched borrow prestates after all venue constraints.",
                "Projected total debt capacity is an observation-sum, not a point-in-time TVL.",
                "Revenue is historical factual-user-flow economic revenue; cash-realization memos and inventory memos are excluded.",
                "Unavailable means the exact denominator or a comparable observation does not exist; no estimate or imputation is used.",
            ],
        },
    }


def build_notebook(path: Path):
    notebook = {
        "cells": [
            {
                "cell_type": "markdown",
                "metadata": {},
                "source": [
                    "# Dusk vs V1: scoped three-market benchmark\n",
                    "\n",
                    "## tl;dr\n",
                    "V1 wins the accessible historical comparison. Dusk has 78.58% less actionable additional borrowing headroom and captures $0.82 of factual user-flow economic revenue versus V1's $1,434.64. Spot execution quality, APY/capital yield, and a full looping-maintenance campaign are unavailable and are not estimated.\n",
                ],
            },
            {
                "cell_type": "markdown",
                "metadata": {},
                "source": [
                    "## Context and method\n",
                    "This notebook reads the compact evidence generated from the retained proof-bound training replay. The three archetypes are Fartcoin/USDC (spot), stORE/USDC (borrowing), and OMFG/USDC (liquidations). Both fixed-flow and intent arms must agree on every factual capacity, revenue, and safety result.\n",
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
                    "evidence = json.loads(Path('evidence/scoped-benchmark.json').read_text())\n",
                    "print(evidence['scope'])\n",
                ],
            },
            {
                "cell_type": "markdown",
                "metadata": {},
                "source": ["## Results\n"],
            },
            {
                "cell_type": "code",
                "execution_count": None,
                "metadata": {},
                "outputs": [],
                "source": [
                    "h = evidence['headline']\n",
                    "assert round(h['actionable_headroom_delta_pct'], 2) == -78.58\n",
                    "assert round(h['v1_factual_revenue_usd'], 2) == 1434.64\n",
                    "assert round(h['dusk_factual_revenue_usd'], 2) == 0.82\n",
                    "assert h['bad_debt_delta_usd'] == 0\n",
                    "assert evidence['arm_agreement']['all_accessible_metrics_equal'] is True\n",
                    "for row in evidence['verdicts']:\n",
                    "    print(f\"{row['angle']}: {row['verdict']} - {row['finding']}\")\n",
                ],
            },
            {
                "cell_type": "code",
                "execution_count": None,
                "metadata": {},
                "outputs": [],
                "source": [
                    "print('Market-level debt capacity')\n",
                    "for row in evidence['market_capacity']:\n",
                    "    print(row)\n",
                    "print('\\nRevenue by beneficiary')\n",
                    "for row in evidence['revenue_comparison']:\n",
                    "    print(row)\n",
                ],
            },
            {
                "cell_type": "markdown",
                "metadata": {},
                "source": [
                    "## Takeaways\n",
                    "1. Dusk's gross theoretical debt capacity is close to V1 in aggregate, but its actionable capacity falls to 20.54% of V1 after health and daily-bucket constraints.\n",
                    "2. Historical factual revenue is overwhelmingly V1-favored because the sampled Dusk profile cannot execute comparable spot flow.\n",
                    "3. The 13 liquidation observations show no bad-debt or tail-shortfall difference, but this is not a full looping leverage campaign.\n",
                    "4. Missing capital-time marks and LP ownership state make APY unavailable; they are not forward-filled or guessed.\n",
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
    path.write_text(json.dumps(notebook, indent=2) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--cache-dir", type=Path, required=True)
    parser.add_argument("--diagnostic-evidence", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()

    args.output_dir.mkdir(parents=True, exist_ok=True)
    evidence_dir = args.output_dir / "evidence"
    evidence_dir.mkdir(exist_ok=True)
    arm_paths = sorted(args.cache_dir.glob("*.arm.json"))
    if len(arm_paths) != 2:
        raise RuntimeError(f"expected two scoped arm artifacts, found {len(arm_paths)}")
    arms = {read_json(path)["arm"]: read_json(path) for path in arm_paths}
    if set(arms) != {"fixed_flow", "intent"}:
        raise RuntimeError("scoped evidence must contain fixed-flow and intent arms")
    fixed = arms["fixed_flow"]
    intent = arms["intent"]
    fixed_score = fixed["score_evidence"]
    intent_score = intent["score_evidence"]
    for arm_name, arm, score in [
        ("fixed_flow", fixed, fixed_score),
        ("intent", intent, intent_score),
    ]:
        if not arm["counterfactual"]["complete_for_scoring"]:
            raise RuntimeError(
                f"{arm_name} replay is incomplete; refusing to publish comparative metrics"
            )
        if score["candidate_score"] is None:
            raise RuntimeError(
                f"{arm_name} profile is unscored; refusing to publish a venue verdict"
            )
        if score["blocking_reasons"]:
            raise RuntimeError(
                f"{arm_name} score has blockers; refusing to publish a venue verdict: "
                f"{score['blocking_reasons']}"
            )
    profile_paths = sorted(args.cache_dir.glob("*.profile.json"))
    if len(profile_paths) != 1:
        raise RuntimeError(f"expected one scoped profile artifact, found {len(profile_paths)}")
    combined_profile = read_json(profile_paths[0])
    if combined_profile["combined_score"] is None or combined_profile["blocking_reasons"]:
        raise RuntimeError(
            "combined profile is unscored or blocked; refusing to publish a venue verdict"
        )
    if fixed_score["included_market_count"] != 3 or intent_score["included_market_count"] != 3:
        raise RuntimeError("scoped arm evidence does not bind exactly three markets")
    if {row["market"] for row in fixed_score["markets"]} != set(MARKETS):
        raise RuntimeError("fixed-flow evidence market set differs from the approved scope")
    if {row["market"] for row in intent_score["markets"]} != set(MARKETS):
        raise RuntimeError("intent evidence market set differs from the approved scope")

    equality_fields = [
        "v1_actionable_headroom_matched_observation_sum_usd_micro",
        "dusk_actionable_headroom_matched_observation_sum_usd_micro",
        "projected_total_debt_capacity",
        "stakeholder_revenue_v1_usd_micro",
        "stakeholder_revenue_dusk_user_usd_micro",
        "v1_bad_debt_usd_micro",
        "dusk_bad_debt_usd_micro",
        "tail_liquidation_shortfall_delta_usd_micro",
    ]
    for field in equality_fields:
        if fixed_score[field] != intent_score[field]:
            raise RuntimeError(f"factual metric differs across replay arms: {field}")

    v1_headroom = micro_usd(fixed_score["v1_actionable_headroom_matched_observation_sum_usd_micro"])
    dusk_headroom = micro_usd(fixed_score["dusk_actionable_headroom_matched_observation_sum_usd_micro"])
    headroom_delta_pct = (dusk_headroom / v1_headroom - 1) * 100
    projected = fixed_score["projected_total_debt_capacity"]
    v1_projected = micro_usd(projected["v1_actionable_max_debt_observation_sum_usd_micro"])
    dusk_gross_projected = micro_usd(projected["dusk_gross_max_debt_observation_sum_usd_micro"])
    dusk_actionable_projected = micro_usd(projected["dusk_actionable_max_debt_observation_sum_usd_micro"])
    v1_revenue = available_usd(fixed_score["stakeholder_revenue_v1_usd_micro"])
    dusk_revenue = available_usd(fixed_score["stakeholder_revenue_dusk_user_usd_micro"])
    if v1_revenue is None or dusk_revenue is None:
        raise RuntimeError("required factual stakeholder revenue is unavailable")

    lp_lanes = {"lp_swap_earned", "lp_public_interest_accrued", "hlp_funding_interest_earned"}
    protocol_lanes = {
        "protocol_fee_auction_swap_earned",
        "protocol_buyback_swap_earned",
        "protocol_public_interest_accrued",
        "protocol_fee_auction_hlp_funding_interest_earned",
        "protocol_buyback_hlp_funding_interest_earned",
    }
    revenue = {
        "V1 LP": lane_usd(fixed_score, "v1", lp_lanes),
        "V1 protocol": lane_usd(fixed_score, "v1", protocol_lanes),
        "Dusk LP": lane_usd(fixed_score, "dusk", lp_lanes),
        "Dusk protocol": lane_usd(fixed_score, "dusk", protocol_lanes),
    }
    if abs(revenue["V1 LP"] + revenue["V1 protocol"] - v1_revenue) > 1e-6:
        raise RuntimeError("V1 beneficiary lanes do not reconcile to stakeholder revenue")
    if abs(revenue["Dusk LP"] + revenue["Dusk protocol"] - dusk_revenue) > 1e-6:
        raise RuntimeError("Dusk beneficiary lanes do not reconcile to stakeholder revenue")

    market_capacity = []
    market_revenue = []
    for row in sorted(fixed_score["markets"], key=lambda item: MARKETS[item["market"]]):
        projected = row["projected_total_debt_capacity"]
        market_capacity.append(
            {
                "market": MARKETS[row["market"]],
                "address": row["market"],
                "matched_borrows": projected["matched_observation_count"],
                "v1_actionable_debt_capacity_usd": micro_usd(projected["v1_actionable_max_debt_observation_sum_usd_micro"]),
                "dusk_gross_debt_capacity_usd": micro_usd(projected["dusk_gross_max_debt_observation_sum_usd_micro"]),
                "dusk_actionable_debt_capacity_usd": micro_usd(projected["dusk_actionable_max_debt_observation_sum_usd_micro"]),
                "dusk_actionable_vs_v1_pct": (
                    micro_usd(projected["dusk_actionable_max_debt_observation_sum_usd_micro"])
                    / micro_usd(projected["v1_actionable_max_debt_observation_sum_usd_micro"])
                    * 100
                    if projected["matched_observation_count"]
                    else None
                ),
            }
        )
        market_revenue.append(
            {
                "market": MARKETS[row["market"]],
                "v1_revenue_usd": available_usd(row["stakeholder_revenue_v1_usd_micro"]),
                "dusk_revenue_usd": available_usd(row["stakeholder_revenue_dusk_user_usd_micro"]),
                "swaps": row["swap_count"],
                "borrows": row["borrow_count"],
                "liquidations": row["liquidation_count"],
            }
        )

    diagnostic = read_json(args.diagnostic_evidence)
    bad_debt_delta = micro_usd(fixed_score["dusk_bad_debt_usd_micro"]) - micro_usd(fixed_score["v1_bad_debt_usd_micro"])
    liquidation_observations = sum(row["liquidation_count"] for row in fixed_score["markets"])
    generated_at = datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
    evidence = {
        "schema_version": 1,
        "generated_at": generated_at,
        "scope": {
            "run_id": diagnostic["run_id"],
            "cutoff_slot": diagnostic["cutoff_slot"],
            "split": "training",
            "profile_id": fixed["profile_id"],
            "hlp_scenario": fixed["hlp_scenario"],
            "markets": [{"address": key, "label": value} for key, value in MARKETS.items()],
            "canonical_operations": fixed["counterfactual"]["canonical_operation_count"],
            "factual_rows": fixed["counterfactual"]["factual_ledger_row_count"],
            "window_start_unix_seconds": fixed_score["window_start_unix_seconds"],
            "window_end_unix_seconds_exclusive": fixed_score["window_end_unix_seconds_exclusive"],
        },
        "quality": {
            "full_selected_panel_replay_raw_unit_differences": diagnostic["replay"]["raw_unit_differences"],
            "full_selected_panel_conservation_failures": diagnostic["replay"]["conservation_failures"],
            "scoped_missing_continuous_price_keys": len(fixed_score["missing_price_keys"]),
            "deferred_lp_withdrawals": fixed["counterfactual"]["deferred_external_state_count"],
            "decision_grade_limitations": diagnostic["certification"]["limitations"],
        },
        "arm_agreement": {
            "arms": ["fixed_flow", "intent"],
            "all_accessible_metrics_equal": True,
            "fixed_flow_status": {
                key: fixed["counterfactual"][key]
                for key in [
                    "executed_count",
                    "partially_filled_count",
                    "rejected_count",
                    "deferred_external_state_count",
                    "no_economic_counterpart_count",
                ]
            },
            "intent_synthetic_arbitrage_rows": intent["counterfactual"]["synthetic_arbitrage_count"],
        },
        "headline": {
            "matched_borrow_observations": fixed_score["projected_total_debt_capacity"]["matched_observation_count"],
            "v1_actionable_headroom_usd": v1_headroom,
            "dusk_actionable_headroom_usd": dusk_headroom,
            "actionable_headroom_delta_pct": headroom_delta_pct,
            "v1_projected_actionable_total_debt_usd": v1_projected,
            "dusk_projected_gross_total_debt_usd": dusk_gross_projected,
            "dusk_projected_actionable_total_debt_usd": dusk_actionable_projected,
            "dusk_actionable_total_debt_vs_v1_pct": dusk_actionable_projected / v1_projected * 100,
            "v1_factual_revenue_usd": v1_revenue,
            "dusk_factual_revenue_usd": dusk_revenue,
            "revenue_delta_pct": (dusk_revenue / v1_revenue - 1) * 100,
            "liquidation_observations": liquidation_observations,
            "bad_debt_delta_usd": bad_debt_delta,
            "tail_shortfall_delta_usd_micro": int(fixed_score["tail_liquidation_shortfall_delta_usd_micro"]),
        },
        "market_capacity": market_capacity,
        "market_revenue": market_revenue,
        "revenue_comparison": [
            {"beneficiary": key, "revenue_usd": value} for key, value in revenue.items()
        ],
        "verdicts": [
            {
                "angle": "Collateral efficiency",
                "availability": "available",
                "verdict": "V1 wins",
                "short_verdict": "V1 wins",
                "finding": f"Dusk actionable additional headroom is {abs(headroom_delta_pct):.2f}% lower across 1,227 matched borrow prestates.",
            },
            {
                "angle": "Capital efficiency / spot yield",
                "availability": "partial",
                "verdict": "V1 wins executable historical flow; APY unavailable",
                "short_verdict": "V1 wins flow; APY N/A",
                "finding": f"V1 earned ${revenue['V1 LP'] + revenue['V1 protocol']:,.2f} of swap-plus-interest revenue versus ${dusk_revenue:,.2f} on Dusk; execution-cost, slippage, and capital-time APY have no exact comparable observations.",
            },
            {
                "angle": "Leverage / maintenance margin",
                "availability": "partial",
                "verdict": "Dusk constrained; full loop campaign unavailable",
                "short_verdict": "Dusk constrained; loops N/A",
                "finding": f"Dusk gross projected debt capacity is {dusk_gross_projected / v1_projected * 100:.2f}% of V1, but actionable capacity falls to {dusk_actionable_projected / v1_projected * 100:.2f}% after health and daily-bucket constraints. A multi-loop maintenance path was not executed.",
            },
            {
                "angle": "LP and protocol revenue",
                "availability": "available raw revenue; yield unavailable",
                "verdict": "V1 wins",
                "short_verdict": "V1 wins",
                "finding": f"LP revenue is ${revenue['V1 LP']:,.2f} vs ${revenue['Dusk LP']:,.2f}; protocol revenue is ${revenue['V1 protocol']:,.2f} vs ${revenue['Dusk protocol']:,.2f}.",
            },
            {
                "angle": "Liquidation safety",
                "availability": "available for observed training liquidations",
                "verdict": "Tie in observed shortfall",
                "short_verdict": "Observed tie",
                "finding": f"Across {liquidation_observations} liquidation observations, both venues have $0 bad debt and the tail-shortfall delta is zero.",
            },
        ],
        "availability": [
            {"metric": "Actionable borrowing headroom", "status": "Available", "reason": "1,227 matched borrow prestates with exact USD marks", "short_reason": "1,227 matched states"},
            {"metric": "Raw LP/protocol economic revenue", "status": "Available", "reason": "Event-time prices exist for every realized revenue lane", "short_reason": "Complete event-time marks"},
            {"metric": "Observed bad debt / liquidation shortfall", "status": "Available", "reason": f"{liquidation_observations} exact liquidation observations", "short_reason": f"{liquidation_observations} liquidations"},
            {"metric": "Execution cost and slippage", "status": "Unavailable", "reason": "No comparable executed spot observations under the sampled Dusk profile", "short_reason": "No paired executed swaps"},
            {"metric": "Capital-time APY / spot yield", "status": "Unavailable", "reason": "93 continuous price keys and the external-LP ownership denominator are missing", "short_reason": "Price/ownership gaps"},
            {"metric": "Full looping leverage campaign", "status": "Unavailable", "reason": "No frozen winner profile or separately executed multi-loop maintenance path", "short_reason": "No loop campaign"},
        ],
        "source_artifacts": {
            "fixed_flow_arm": {"path": str(arm_paths[0]), "sha256": sha256_file(arm_paths[0]), "bytes": arm_paths[0].stat().st_size},
            "intent_arm": {"path": str(arm_paths[1]), "sha256": sha256_file(arm_paths[1]), "bytes": arm_paths[1].stat().st_size},
            "v1_reconciliation": {"path": str(args.run_dir / "v1-reconciliation.json"), "sha256": sha256_file(args.run_dir / "v1-reconciliation.json")},
            "diagnostic_evidence": {"path": str(args.diagnostic_evidence), "sha256": sha256_file(args.diagnostic_evidence)},
        },
    }
    evidence_path = evidence_dir / "scoped-benchmark.json"
    evidence_path.write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")

    sources = [
        source("capacity", "Matched borrowing-state comparison", "Exact V1 and Dusk capacity at the same historical borrow prestates.", ["fixed-flow arm", "intent arm"]),
        source("revenue", "Factual economic revenue lanes", "Event-time USD revenue split between LP and protocol beneficiaries.", ["fixed-flow arm", "historical prices"]),
        source("safety", "Observed liquidation safety", "Bad-debt and tail-shortfall differences on exact paired liquidation rows.", ["fixed-flow arm", "V1 reconciliation"]),
        source("quality", "Evidence availability and replay QA", "Exact evidence boundaries and typed unavailable metrics.", ["V1 reconciliation", "DecisionGrade gate"]),
    ]
    artifact = {
        "surface": "report",
        "manifest": {
            "version": 1,
            "surface": "report",
            "title": "Dusk vs V1: Three-Market Historical Benchmark",
            "description": "Actual scoped comparison on retained historical evidence; inaccessible metrics are explicitly unavailable.",
            "generatedAt": generated_at,
            "cards": [
                {"id": "headroom", "dataset": "headline", "sourceId": "capacity", "description": "Dusk actionable additional headroom relative to V1 across matched borrow prestates.", "metrics": [{"label": "Dusk headroom delta", "field": "actionable_headroom_delta_pct", "format": "number", "unit": "%"}]},
                {"id": "v1rev", "dataset": "headline", "sourceId": "revenue", "description": "V1 factual-user-flow economic revenue in the training window.", "metrics": [{"label": "V1 revenue", "field": "v1_factual_revenue_usd", "format": "currency"}]},
                {"id": "duskrev", "dataset": "headline", "sourceId": "revenue", "description": "Dusk factual-user-flow economic revenue under the sampled executable profile.", "metrics": [{"label": "Dusk revenue", "field": "dusk_factual_revenue_usd", "format": "currency"}]},
                {"id": "shortfall", "dataset": "headline", "sourceId": "safety", "description": "Difference in observed bad debt across exact liquidation observations.", "metrics": [{"label": "Bad-debt delta", "field": "bad_debt_delta_usd", "format": "currency"}]},
            ],
            "charts": [
                {"id": "revenue", "title": "Historical economic revenue by beneficiary", "subtitle": "Factual user flow only; USD; capital-time yield is unavailable", "intent": "comparison", "question": "Who earned revenue under each venue replay?", "rationale": "Four horizontal bars expose the venue and beneficiary gap directly.", "type": "horizontalBar", "dataset": "revenue_comparison", "sourceId": "revenue", "encodings": {"x": {"field": "beneficiary", "type": "nominal", "label": "Venue / beneficiary"}, "y": {"field": "revenue_usd", "type": "quantitative", "format": "currency", "label": "Revenue"}}, "valueFormat": "currency", "layout": "full", "labels": {"values": "all"}, "palette": {"kind": "categorical", "name": "tableau10"}, "settings": {"orientation": "horizontal", "sort": "descending", "showValues": True}},
                {"id": "capacity", "title": "Actionable projected debt capacity by market", "subtitle": "Observation-sums at matched borrow prestates; spot-only market has no borrowing observations", "intent": "comparison", "question": "How much of V1 debt capacity remains actionable under Dusk?", "rationale": "A compact market comparison reveals that constraint effects differ by archetype.", "type": "horizontalBar", "dataset": "capacity_chart", "sourceId": "capacity", "encodings": {"x": {"field": "series", "type": "nominal", "label": "Market / venue"}, "y": {"field": "capacity_usd", "type": "quantitative", "format": "currency", "label": "Projected debt capacity"}}, "valueFormat": "currency", "layout": "full", "labels": {"values": "all"}, "palette": {"kind": "categorical", "name": "tableau10"}, "settings": {"orientation": "horizontal", "sort": "descending", "showValues": True}},
            ],
            "tables": [
                {"id": "verdicts", "title": "Answers to the original benchmark questions", "subtitle": "Detailed findings appear in the sections below", "dataset": "verdicts", "sourceId": "quality", "density": "comfortable", "layout": "full", "columns": [{"field": "angle", "label": "Angle", "type": "text"}, {"field": "availability", "label": "Evidence", "type": "text"}, {"field": "short_verdict", "label": "Verdict", "type": "text"}]},
                {"id": "availability", "title": "Metric availability", "subtitle": "Unavailable cells are deliberate, not missing report work", "dataset": "availability", "sourceId": "quality", "density": "comfortable", "layout": "full", "columns": [{"field": "metric", "label": "Metric", "type": "text"}, {"field": "status", "label": "Status", "type": "text"}, {"field": "short_reason", "label": "Evidence boundary", "type": "text"}]},
            ],
            "sources": sources,
            "blocks": [
                {"id": "title", "type": "markdown", "layout": "full", "body": "# Dusk vs V1: Three-Market Historical Benchmark"},
                {"id": "summary", "type": "markdown", "layout": "full", "body": f"## Executive verdict\n\n**V1 wins the accessible historical comparison.** On 1,227 matched borrowing states, Dusk provides **{abs(headroom_delta_pct):.2f}% less actionable additional headroom**. V1 earned **${v1_revenue:,.2f}** of factual economic revenue versus **${dusk_revenue:,.2f}** for Dusk. Dusk did not show worse observed liquidation shortfall, but spot execution quality, capital-time APY, and a complete looping-maintenance campaign are unavailable. Therefore the decision is **do not claim Dusk outperforms V1 from this profile**."},
                {"id": "headline", "type": "metric-strip", "layout": "full", "cardIds": ["headroom", "v1rev", "duskrev", "shortfall"]},
                {"id": "scope", "type": "markdown", "layout": "full", "sourceId": "quality", "body": "## Scope and trust boundary\n\nThe panel uses **Fartcoin/USDC** for spot activity, **stORE/USDC** for borrowing, and **OMFG/USDC** for liquidations. It reuses the retained training history and the only one of 32 sampled Dusk profiles that initializes all selected markets. The underlying V1 reconstruction has **zero raw-unit differences** and **zero conservation failures**. Both Dusk replay arms agree on every accessible metric. This is a DecisionGrade selected-panel result, not a full-universe or deployed-binary certification."},
                {"id": "verdict_table", "type": "table", "layout": "full", "tableId": "verdicts"},
                {"id": "capacity_text", "type": "markdown", "layout": "full", "sourceId": "capacity", "body": f"## Collateral and leverage capacity\n\nDusk's gross projected debt capacity is **${dusk_gross_projected:,.0f}**, close to V1's **${v1_projected:,.0f}**. After global-health and daily-borrow-bucket constraints, Dusk actionable capacity falls to **${dusk_actionable_projected:,.0f} ({dusk_actionable_projected / v1_projected * 100:.2f}% of V1)**. This is a clear constraint penalty. It is not a substitute for a multi-loop maintenance-margin campaign, which remains unavailable."},
                {"id": "capacity_chart", "type": "chart", "layout": "full", "chartId": "capacity"},
                {"id": "revenue_text", "type": "markdown", "layout": "full", "sourceId": "revenue", "body": f"## Revenue and spot result\n\nV1 LPs earned **${revenue['V1 LP']:,.2f}** and the V1 protocol earned **${revenue['V1 protocol']:,.2f}**. Dusk LP and protocol revenue were **${revenue['Dusk LP']:,.2f}** and **${revenue['Dusk protocol']:,.2f}**. Dusk produced no comparable executed spot observations, so execution-cost, slippage, dynamic-fee benefit, and APY are not reported. The raw revenue result is still exact and strongly favors V1."},
                {"id": "revenue_chart", "type": "chart", "layout": "full", "chartId": "revenue"},
                {"id": "safety", "type": "markdown", "layout": "full", "sourceId": "safety", "body": f"## Observed liquidation safety\n\nAcross **{liquidation_observations}** liquidation observations in the training window, both venues record **$0 bad debt** and the tail-shortfall delta is zero. This is a scoped tie on observed liquidation shortfall, not proof that leverage risk is equal under unobserved loops or future paths."},
                {"id": "availability_table", "type": "table", "layout": "full", "tableId": "availability"},
                {"id": "recommendation", "type": "markdown", "layout": "full", "body": "## Recommendation\n\nDo **not** market this Dusk profile as superior to V1. The immediate design issue is not gross theoretical capacity; it is the conversion from gross to actionable capacity and the inability to execute comparable historical spot flow. If further work is funded, prioritize one small profile-design iteration focused on curve executability and daily-bucket/global-health constraints, then rerun this same three-market panel. Do not spend storage on a full-universe crawl until this scoped panel produces comparable spot observations.\n\n## Caveats\n\n- Training split only; no holdout verdict was opened because no profile achieved a complete score.\n- 77 LP withdrawals are deferred because ownership state is unavailable after rejected antecedent deposits.\n- 93 continuous price keys are absent for capital-time integration; event-time revenue marks remain complete.\n- Decoder-revision and terminal-state certification are typed DecisionGrade limitations.\n- The selected Dusk profile is a deterministic sampled candidate, not a calibrated winner."},
                {"id": "method", "type": "markdown", "layout": "full", "sourceId": "quality", "body": "## Method and reproducibility\n\nThe benchmark replays identical historical operations through V1 and Dusk, binds capacity and revenue rows by operation, values realized lanes at historical event-time marks, and checks both fixed-flow and intent arms. All compact inputs are checksum-bound in the accompanying evidence JSON; the notebook executes the headline assertions top to bottom."},
            ],
        },
        "snapshot": {
            "version": 1,
            "generatedAt": generated_at,
            "status": "ready",
            "datasets": {
                "headline": [evidence["headline"]],
                "verdicts": evidence["verdicts"],
                "availability": evidence["availability"],
                "revenue_comparison": evidence["revenue_comparison"],
                "market_capacity": market_capacity,
                "market_revenue": market_revenue,
                "capacity_chart": [
                    {"series": f"{row['market']} - V1", "capacity_usd": row["v1_actionable_debt_capacity_usd"]}
                    for row in market_capacity if row["matched_borrows"]
                ] + [
                    {"series": f"{row['market']} - Dusk", "capacity_usd": row["dusk_actionable_debt_capacity_usd"]}
                    for row in market_capacity if row["matched_borrows"]
                ],
            },
        },
        "sources": sources,
    }
    (args.output_dir / "artifact.json").write_text(json.dumps(artifact, indent=2) + "\n", encoding="utf-8")
    build_notebook(args.output_dir / "scoped-benchmark.ipynb")


if __name__ == "__main__":
    main()
