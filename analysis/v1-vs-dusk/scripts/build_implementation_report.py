#!/usr/bin/env python3
"""Build the canonical portable artifact for the benchmark implementation report.

This report intentionally describes implementation and verification status. It
does not fabricate a ResultManifestV1 or any Dusk-versus-V1 performance result.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any, Iterable, Sequence


BENCHMARK_ROOT = Path(__file__).resolve().parent.parent
REPOSITORY_ROOT = BENCHMARK_ROOT.parent.parent
OUTPUT_DIRECTORY = BENCHMARK_ROOT / "reports" / "implementation-status"
ARTIFACT_PATH = OUTPUT_DIRECTORY / "artifact.json"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_atomic(path: Path, payload: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.partial")
    temporary.write_text(payload, encoding="utf-8")
    temporary.replace(path)


def sql_literal(value: Any) -> str:
    if isinstance(value, bool):
        return "TRUE" if value else "FALSE"
    if isinstance(value, int):
        return str(value)
    if value is None:
        return "NULL"
    return "'" + str(value).replace("'", "''") + "'"


def values_sql(
    dataset: str,
    columns: Sequence[str],
    rows: Iterable[dict[str, Any]],
    order_by: str,
) -> str:
    encoded_rows = list(rows)
    values = ",\n".join(
        "    (" + ", ".join(sql_literal(row[column]) for column in columns) + ")"
        for row in encoded_rows
    )
    column_list = ", ".join(columns)
    return (
        "-- Generated implementation-status evidence for the Dusk versus V1 benchmark.\n"
        "-- This is implementation evidence, not historical performance data.\n"
        f"WITH {dataset}({column_list}) AS (\n"
        "  VALUES\n"
        f"{values}\n"
        ")\n"
        f"SELECT * FROM {dataset} ORDER BY {order_by};\n"
    )


def source(
    *,
    source_id: str,
    label: str,
    file_name: str,
    sql: str,
    description: str,
    tables_used: list[str],
    filters: list[str],
    metric_definitions: list[str],
) -> dict[str, Any]:
    write_atomic(OUTPUT_DIRECTORY / file_name, sql)
    return {
        "id": source_id,
        "label": label,
        "path": file_name,
        "query": {
            "engine": "duckdb",
            "id": f"dusk_v1_{source_id}_20260821",
            "sql": sql,
            "description": description,
            "language": "sql",
            "tables_used": tables_used,
            "filters": filters,
            "metric_definitions": metric_definitions,
        },
    }


def text_column(field: str, label: str, align: str = "left") -> dict[str, str]:
    return {"field": field, "label": label, "type": "text", "align": align}


def number_column(field: str, label: str) -> dict[str, str]:
    return {"field": field, "label": label, "type": "number", "align": "right"}


def table(
    *,
    table_id: str,
    title: str,
    subtitle: str,
    dataset: str,
    source_id: str,
    sort_field: str,
    sort_direction: str,
    columns: list[dict[str, str]],
    density: str = "compact",
) -> dict[str, Any]:
    return {
        "id": table_id,
        "title": title,
        "subtitle": subtitle,
        "dataset": dataset,
        "density": density,
        "sourceId": source_id,
        "layout": "full",
        "defaultSort": {"field": sort_field, "direction": sort_direction},
        "columns": columns,
    }


def markdown(block_id: str, body: str) -> dict[str, str]:
    return {"type": "markdown", "id": block_id, "body": body}


def main() -> int:
    verification_counts = [
        {
            "suite": "Benchmark library tests",
            "verified_items": 164,
            "evidence_kind": "Rust unit tests",
            "result": "passed",
        },
        {
            "suite": "Dusk native benchmark tests",
            "verified_items": 22,
            "evidence_kind": "Native parity and rollback tests",
            "result": "passed",
        },
        {
            "suite": "Schema contracts",
            "verified_items": 18,
            "evidence_kind": "16 artifact snapshots plus 2 supporting schemas",
            "result": "passed",
        },
        {
            "suite": "Data-quality integration tests",
            "verified_items": 14,
            "evidence_kind": "Extraction and audit fixtures",
            "result": "passed",
        },
        {
            "suite": "Portable report audiences",
            "verified_items": 3,
            "evidence_kind": "Chromium desktop and narrow-layout verification",
            "result": "passed",
        },
    ]

    verification_commands = [
        {
            "layer": "Benchmark workspace",
            "command": "CARGO_INCREMENTAL=0 cargo test --locked --offline --manifest-path analysis/v1-vs-dusk/Cargo.toml --no-fail-fast",
            "result": "164 library + 14 data-quality passed; 0 failed",
        },
        {
            "layer": "All analysis targets",
            "command": "CARGO_INCREMENTAL=0 cargo check --locked --offline --manifest-path analysis/v1-vs-dusk/Cargo.toml --all-targets",
            "result": "passed",
        },
        {
            "layer": "Dusk native benchmark API",
            "command": "CARGO_INCREMENTAL=0 cargo test --locked --offline -p dusk --features benchmark benchmark_api::tests --no-fail-fast",
            "result": "22 passed; 0 failed",
        },
        {
            "layer": "Default Dusk build",
            "command": "CARGO_INCREMENTAL=0 cargo check --locked --offline -p dusk",
            "result": "passed with benchmark feature disabled",
        },
        {
            "layer": "Dusk benchmark build",
            "command": "CARGO_INCREMENTAL=0 cargo check --locked --offline -p dusk --features benchmark",
            "result": "passed",
        },
        {
            "layer": "Schema catalog",
            "command": "PYTHONDONTWRITEBYTECODE=1 python3 analysis/v1-vs-dusk/schemas/validate.py --self-test",
            "result": "16 artifact snapshots + 2 supporting schemas passed",
        },
        {
            "layer": "Notebook executor",
            "command": "PYTHONDONTWRITEBYTECODE=1 python3 analysis/v1-vs-dusk/scripts/execute_notebook.py --self-test",
            "result": "checksum, identity, duplicate-key, winner, exact-value, sandbox, and determinism passed",
        },
        {
            "layer": "Portable reports",
            "command": "cargo test ... caller_supplied_canonical_builder_accepts_all_three_artifacts -- --ignored",
            "result": "technical, governance, and public audiences passed Chromium verification",
        },
    ]

    pipeline_stages = [
        {
            "stage_order": 1,
            "stage": "extract",
            "purpose": "Export a repeatable-read database snapshot and enrich it with finalized archival RPC evidence.",
            "primary_artifacts": "Ordered operations/events, configs, prices, transfer-fee proofs, Parquet",
            "acceptance_rule": "Complete certified signature/page/block chains and exact typed decoding",
        },
        {
            "stage_order": 2,
            "stage": "audit",
            "purpose": "Reconcile DB and RPC sources and calculate global plus per-market eligibility.",
            "primary_artifacts": "data-quality.json and coverage evidence",
            "acceptance_rule": "Every non-USD hard gate passes; missing prices only remove USD claims",
        },
        {
            "stage_order": 3,
            "stage": "reconcile-v1",
            "purpose": "Replay V1 natively and prove terminal raw-unit equality at the cutoff.",
            "primary_artifacts": "V1 replay ledger, terminal proof, universe/common window, 60/20/20 splits",
            "acceptance_rule": "Zero raw-unit differences plus exact-cutoff terminal snapshot equality",
        },
        {
            "stage_order": 4,
            "stage": "calibrate",
            "purpose": "Fit the training-only interest schedule and search global/per-market Dusk profiles.",
            "primary_artifacts": "Interest schedule, calibration evidence, Pareto set, profile freeze",
            "acceptance_rule": "Validation gates pass and holdout remains sealed before freeze",
        },
        {
            "stage_order": 5,
            "stage": "evaluate",
            "purpose": "Replay the sealed holdout across both behavior arms and both hLP scenarios.",
            "primary_artifacts": "Paired metrics, leverage campaigns, sensitivity, metrics manifest",
            "acceptance_rule": "Safety/execution gates and conservation identities remain exact",
        },
        {
            "stage_order": 6,
            "stage": "verify",
            "purpose": "Recheck every manifest/source hash and execute the bounded notebook.",
            "primary_artifacts": "verification.json and benchmark.executed.ipynb",
            "acceptance_rule": "Manifest, notebook sandbox, exact values, and source commitments pass",
        },
        {
            "stage_order": 7,
            "stage": "render",
            "purpose": "Build technical, governance, and public portable reports from one manifest.",
            "primary_artifacts": "artifact.json, provenance.sql, report.html, render receipt",
            "acceptance_rule": "Canonical packaging and Chromium desktop/mobile verification pass",
        },
    ]

    benchmark_angles = [
        {
            "angle": "Collateral efficiency",
            "factual_control": "V1 debt, collateral, capacity, and observed debt-action fractions",
            "dusk_counterfactual": "Native gross health ceiling plus actionable cash/daily/global/reduce-only bounds with exact share rounding",
            "headline_outputs": "Gross/actionable headroom, capacity multiplier, filled/unmet demand, limiting constraints",
            "implementation_status": "implemented and tested; historical result unavailable",
        },
        {
            "angle": "Spot capital efficiency",
            "factual_control": "Every historical exact-in swap, fee split, min-out, and reserve checkpoint",
            "dusk_counterfactual": "Concentrated curve, same-slot dynamic fees, recentering, hLP settlement, and intent arbitrage",
            "headline_outputs": "Execution cost, slippage, equivalent depth, supported volume, fee return, inventory PnL",
            "implementation_status": "implemented and tested; historical result unavailable",
        },
        {
            "angle": "Leverage",
            "factual_control": "Bounded V1 borrow-swap-deposit loops from identical historical pre-state",
            "dusk_counterfactual": "Native isolated open/full unwind at 72 cells per eligible lending market",
            "headline_outputs": "Executable leverage, opening/unwind cost, funding APR, thresholds, buffers, realized MAE",
            "implementation_status": "implemented and tested; historical result unavailable",
        },
        {
            "angle": "LP and protocol economics",
            "factual_control": "V1 native fee, interest, claim, flashloan, cash-covered surcharge, and inventory transitions",
            "dusk_counterfactual": "Exact swap/public-interest/hLP/recenter lanes with user and synthetic origins separated",
            "headline_outputs": "15 lane totals, common-capital return, >=30-day APR, cash/inventory memos, shortfall",
            "implementation_status": "implemented and tested; historical result unavailable",
        },
    ]

    revenue_lanes = [
        ("lp_swap_earned", "economic_revenue", "yes", "LP swap-fee economics"),
        ("protocol_fee_auction_swap_earned", "economic_revenue", "yes", "Protocol swap fee-auction lane"),
        ("protocol_buyback_swap_earned", "economic_revenue", "yes", "Protocol swap buyback lane"),
        ("retained_recenter_inventory", "inventory_memo", "no", "Protected inventory until economically realized"),
        ("compounded_lp_swap_principal", "principal_disposition_memo", "no", "LP fee amount capitalized into principal"),
        ("lp_public_interest_accrued", "economic_revenue", "yes", "LP public-borrow interest accrual"),
        ("protocol_public_interest_accrued", "economic_revenue", "yes", "Protocol public-borrow interest accrual"),
        ("lp_public_interest_cash_realized", "cash_realization_memo", "no", "Cash realization; V1 remains typed unavailable"),
        ("protocol_fee_auction_public_interest_cash_realized", "cash_realization_memo", "no", "Fee-auction public-interest cash"),
        ("protocol_buyback_public_interest_cash_realized", "cash_realization_memo", "no", "Buyback public-interest cash"),
        ("hlp_funding_interest_earned", "economic_revenue", "yes", "Net hLP funding earned by hLP providers"),
        ("protocol_fee_auction_hlp_funding_interest_earned", "economic_revenue", "yes", "Protocol fee-auction share of hLP funding"),
        ("protocol_buyback_hlp_funding_interest_earned", "economic_revenue", "yes", "Protocol buyback share of hLP funding"),
        ("flashloan_fee_v1_only", "venue_only_supplement", "no", "V1-only supplement outside apples-to-apples core"),
        ("protocol_claim_realized_v1_only", "cash_realization_memo", "no", "V1 claim realization, never new revenue"),
    ]
    revenue_rows = [
        {
            "lane": lane,
            "accounting_class": accounting_class,
            "economic_core": core,
            "interpretation": interpretation,
        }
        for lane, accounting_class, core, interpretation in revenue_lanes
    ]

    blockers = [
        {
            "blocker": "Historical decoder dispatch is provisional",
            "current_state": "One all-slot provisional v0.10.5 decoder; headline_verified=false",
            "why_it_blocks": "Historical upgrades cannot be attributed to exact deployed binary/IDL revisions from labels alone.",
            "required_evidence": "Sealed Program/ProgramData/buffer histories, reconstructed deployed ELF hashes, and order-aware revision decoders",
        },
        {
            "blocker": "Exact terminal snapshot is not supplied",
            "current_state": "Runner validates but deliberately does not fabricate or fetch current-state substitutes",
            "why_it_blocks": "Transition replay cannot be accepted until every terminal account equals the exact cutoff state.",
            "required_evidence": "Independent binding plus full requested account set with response_context_slot exactly equal to cutoff",
        },
    ]

    source_files = [
        (BENCHMARK_ROOT / "README.md", "Operator entry point and current status"),
        (BENCHMARK_ROOT / "docs" / "METHODOLOGY.md", "Normative benchmark methodology"),
        (BENCHMARK_ROOT / "src" / "pipeline.rs", "Seven-stage orchestration"),
        (REPOSITORY_ROOT / "programs" / "dusk" / "src" / "benchmark_api.rs", "Feature-only native transition wrappers"),
        (BENCHMARK_ROOT / "notebooks" / "benchmark.ipynb", "Bounded reader-facing notebook"),
        (BENCHMARK_ROOT / "scripts" / "execute_notebook.py", "Restricted notebook validator/executor"),
        (BENCHMARK_ROOT / "schemas" / "catalog.v1.json", "Strict schema snapshot catalog"),
    ]
    source_inventory = [
        {
            "artifact": path.relative_to(REPOSITORY_ROOT).as_posix(),
            "purpose": purpose,
            "sha256": sha256_file(path),
        }
        for path, purpose in source_files
    ]

    datasets = {
        "verification_counts": verification_counts,
        "verification_commands": verification_commands,
        "pipeline_stages": pipeline_stages,
        "benchmark_angles": benchmark_angles,
        "revenue_lanes": revenue_rows,
        "blockers": blockers,
        "source_inventory": source_inventory,
    }

    source_specs = [
        (
            "verification_evidence",
            "Verified offline test and rendering evidence",
            "verification-evidence.sql",
            "verification_counts",
            ["suite", "verified_items", "evidence_kind", "result"],
            verification_counts,
            "verified_items DESC",
            "Frozen counts from the final locked/offline verification run.",
            ["analysis/v1-vs-dusk/Cargo.toml", "programs/dusk/Cargo.toml"],
            ["Commands executed on 21 August 2026", "No live RPC or database writes"],
            ["verified_items: independently passed tests, schema artifacts, or report audiences in the named layer"],
        ),
        (
            "verification_commands",
            "Reproducible verification commands",
            "verification-commands.sql",
            "verification_commands",
            ["layer", "command", "result"],
            verification_commands,
            "layer",
            "Exact commands and observed final status for each verification layer.",
            ["analysis/v1-vs-dusk/Cargo.lock", "programs/dusk/Cargo.toml"],
            ["Locked and offline Cargo resolution", "CARGO_INCREMENTAL=0 for bounded disk usage"],
            ["result: final process outcome observed by the implementation run"],
        ),
        (
            "pipeline_contract",
            "Seven-stage benchmark pipeline contract",
            "pipeline-contract.sql",
            "pipeline_stages",
            ["stage_order", "stage", "purpose", "primary_artifacts", "acceptance_rule"],
            pipeline_stages,
            "stage_order",
            "Ordered stage responsibilities and fail-closed acceptance rules.",
            ["analysis/v1-vs-dusk/src/pipeline.rs", "analysis/v1-vs-dusk/README.md"],
            ["Canonical stage order only", "No production deployment action"],
            ["acceptance_rule: evidence that must exist before the next stage may consume a receipt"],
        ),
        (
            "benchmark_contract",
            "Requested benchmark-angle implementation contract",
            "benchmark-contract.sql",
            "benchmark_angles",
            ["angle", "factual_control", "dusk_counterfactual", "headline_outputs", "implementation_status"],
            benchmark_angles,
            "angle",
            "Mapping from each requested decision angle to factual control, counterfactual, and outputs.",
            ["analysis/v1-vs-dusk/docs/METHODOLOGY.md"],
            ["Historical counterfactual only", "No certified ResultManifestV1 exists"],
            ["implementation_status: code/test readiness, never a Dusk-versus-V1 performance conclusion"],
        ),
        (
            "revenue_contract",
            "Exhaustive revenue-lane accounting contract",
            "revenue-contract.sql",
            "revenue_lanes",
            ["lane", "accounting_class", "economic_core", "interpretation"],
            revenue_rows,
            "lane",
            "All 15 stable revenue lanes and their non-double-counting classes.",
            ["analysis/v1-vs-dusk/src/metric_engine.rs"],
            ["Three mutually exclusive origins retained separately", "Cash, inventory, principal, claims, and flash never enter economic core"],
            ["economic_core=yes only for economic_revenue accounting-class lanes"],
        ),
        (
            "blocking_evidence",
            "Mandatory evidence blockers",
            "blocking-evidence.sql",
            "blockers",
            ["blocker", "current_state", "why_it_blocks", "required_evidence"],
            blockers,
            "blocker",
            "The two unresolved inputs that intentionally prevent historical result publication.",
            ["analysis/v1-vs-dusk/decoder-revisions.toml", "analysis/v1-vs-dusk/src/v1_terminal_reconciliation.rs"],
            ["No gate relaxation", "No current-state substitution"],
            ["required_evidence: minimum external proof needed to unblock the certified campaign"],
        ),
        (
            "source_inventory",
            "Source artifact checksum inventory",
            "source-inventory.sql",
            "source_inventory",
            ["artifact", "purpose", "sha256"],
            source_inventory,
            "artifact",
            "SHA-256 inventory of the principal implementation and documentation inputs.",
            [row[0].relative_to(REPOSITORY_ROOT).as_posix() for row in source_files],
            ["Current working-tree bytes", "Generated report outputs excluded"],
            ["sha256: lowercase SHA-256 of the exact local file bytes used for this report"],
        ),
    ]

    sources: list[dict[str, Any]] = []
    for (
        source_id,
        label,
        file_name,
        dataset,
        columns,
        rows,
        order_by,
        description,
        tables_used,
        filters,
        definitions,
    ) in source_specs:
        sources.append(
            source(
                source_id=source_id,
                label=label,
                file_name=file_name,
                sql=values_sql(dataset, columns, rows, order_by),
                description=description,
                tables_used=tables_used,
                filters=filters,
                metric_definitions=definitions,
            )
        )

    charts = [
        {
            "id": "verification_layer_counts",
            "title": "Passed verification items by layer",
            "subtitle": "Heterogeneous implementation checks; these are not Dusk-versus-V1 performance observations.",
            "type": "horizontalBar",
            "dataset": "verification_counts",
            "sourceId": "verification_evidence",
            "encodings": {
                "x": {"field": "suite", "type": "nominal", "label": "Verification layer"},
                "y": {
                    "field": "verified_items",
                    "type": "quantitative",
                    "label": "Passed items",
                    "unit": "items",
                },
                "tooltip": [
                    {"field": "suite", "type": "text", "label": "Layer"},
                    {"field": "verified_items", "type": "number", "label": "Passed items"},
                    {"field": "evidence_kind", "type": "text", "label": "Evidence kind"},
                    {"field": "result", "type": "text", "label": "Result"},
                ],
            },
            "xAxisTitle": "Verification layer",
            "yAxisTitle": "Passed items",
            "valueFormat": "number",
            "unit": "items",
            "layout": "full",
            "maxRows": len(verification_counts),
        }
    ]

    tables = [
        table(
            table_id="verification_commands_table",
            title="Reproducible verification commands",
            subtitle="Final locked/offline commands and observed outcomes from 21 August 2026.",
            dataset="verification_commands",
            source_id="verification_commands",
            sort_field="layer",
            sort_direction="asc",
            columns=[
                text_column("layer", "Layer"),
                text_column("command", "Command"),
                text_column("result", "Observed result"),
            ],
        ),
        table(
            table_id="benchmark_angles_table",
            title="Requested benchmark angles",
            subtitle="Each comparison is implemented, but no historical outcome is published without a sealed result manifest.",
            dataset="benchmark_angles",
            source_id="benchmark_contract",
            sort_field="angle",
            sort_direction="asc",
            columns=[
                text_column("angle", "Angle"),
                text_column("factual_control", "V1 factual control"),
                text_column("dusk_counterfactual", "Dusk counterfactual"),
                text_column("headline_outputs", "Outputs"),
                text_column("implementation_status", "Status"),
            ],
        ),
        table(
            table_id="pipeline_stages_table",
            title="Immutable seven-stage pipeline",
            subtitle="A stage receipt is consumable only after its exact acceptance rule passes.",
            dataset="pipeline_stages",
            source_id="pipeline_contract",
            sort_field="stage_order",
            sort_direction="asc",
            columns=[
                number_column("stage_order", "#"),
                text_column("stage", "Stage"),
                text_column("purpose", "Purpose"),
                text_column("primary_artifacts", "Primary artifacts"),
                text_column("acceptance_rule", "Acceptance rule"),
            ],
        ),
        table(
            table_id="revenue_lanes_table",
            title="Exhaustive revenue accounting lanes",
            subtitle="Economic accrual is additive; cash, inventory, principal disposition, claims, and flash supplements are memos or venue-only evidence.",
            dataset="revenue_lanes",
            source_id="revenue_contract",
            sort_field="lane",
            sort_direction="asc",
            columns=[
                text_column("lane", "Lane"),
                text_column("accounting_class", "Accounting class"),
                text_column("economic_core", "In stakeholder core"),
                text_column("interpretation", "Interpretation"),
            ],
        ),
        table(
            table_id="source_inventory_table",
            title="Reproducibility source inventory",
            subtitle="Exact working-tree file hashes used to assemble this implementation-status report.",
            dataset="source_inventory",
            source_id="source_inventory",
            sort_field="artifact",
            sort_direction="asc",
            columns=[
                text_column("artifact", "Artifact"),
                text_column("purpose", "Purpose"),
                text_column("sha256", "SHA-256"),
            ],
        ),
        table(
            table_id="blockers_table",
            title="Evidence still required before historical results",
            subtitle="Both blockers are fail-closed; neither can be replaced with labels, current state, or approximate data.",
            dataset="blockers",
            source_id="blocking_evidence",
            sort_field="blocker",
            sort_direction="asc",
            columns=[
                text_column("blocker", "Blocker"),
                text_column("current_state", "Current state"),
                text_column("why_it_blocks", "Why it blocks"),
                text_column("required_evidence", "Required evidence"),
            ],
            density="spacious",
        ),
    ]

    blocks = [
        markdown("title", "# Dusk versus V1 historical benchmark: implementation and audit report"),
        markdown(
            "technical_summary",
            "## The benchmark system is ready; the historical verdict is not yet certifiable\n\n"
            "The complete extraction, factual replay, Dusk counterfactual, calibration, metric, sensitivity, notebook, and reporting stack is implemented and verified. "
            "No Dusk-versus-V1 winner or performance number appears in this report because two mandatory evidence inputs are still absent. This is the intended fail-closed outcome, not a substituted estimate.",
        ),
        markdown(
            "verification_intro",
            "## Independent verification covers the critical implementation layers\n\n"
            "The chart summarizes passed items across five different verification layers. Counts are intentionally not combined into one quality score; the value is breadth across native transition parity, benchmark logic, extraction quality, schemas, and rendered reports.",
        ),
        {"type": "chart", "id": "verification_chart", "chartId": "verification_layer_counts"},
        markdown(
            "verification_commands_intro",
            "## Every verification result is reproducible offline\n\n"
            "The command table records the exact locked/offline entry points. The only browser-dependent check is the portable report verifier; it passed all three report audiences at desktop and narrow widths.",
        ),
        {"type": "table", "id": "verification_commands", "tableId": "verification_commands_table"},
        markdown(
            "angles_intro",
            "## All four requested decision angles have an exact metric contract\n\n"
            "The factual side always comes from native V1 history. The Dusk side replays the same ordered flow through feature-only native transitions. Unavailable behavior is rejected or typed N/A rather than filled with synthetic cash, volume, ownership, or prices.",
        ),
        {"type": "table", "id": "benchmark_angles", "tableId": "benchmark_angles_table"},
        markdown(
            "pipeline_intro",
            "## The pipeline prevents holdout leakage and partial-evidence promotion\n\n"
            "Seven immutable stages separate extraction, acceptance, factual reconciliation, training, holdout evaluation, verification, and presentation. Each receipt binds the cutoff, source identity, configuration, and predecessor state.",
        ),
        {"type": "table", "id": "pipeline_stages", "tableId": "pipeline_stages_table"},
        markdown(
            "revenue_intro",
            "## Revenue is exhaustive without double counting\n\n"
            "Fifteen token-side lanes are preserved separately for V1 and Dusk. Factual user flow, synthetic arbitrage, and synthetic protocol settlement are mutually exclusive origins. Only economic-revenue lanes enter stakeholder totals; realization and balance-sheet movements remain visible but non-additive.",
        ),
        {"type": "table", "id": "revenue_lanes", "tableId": "revenue_lanes_table"},
        markdown(
            "source_inventory_intro",
            "## Reproducibility is bound to exact source bytes\n\n"
            "The source inventory fingerprints the principal runner, native API, methodology, notebook, executor, and schema catalog. A later source change invalidates these hashes and requires regeneration.",
        ),
        {"type": "table", "id": "source_inventory", "tableId": "source_inventory_table"},
        markdown(
            "limitations_intro",
            "## Two external evidence gaps block the real historical report\n\n"
            "Historical decoder provenance and terminal account equality are correctness requirements. Until both are independently supplied and validated, the runner must stop before calibration/evaluation and no report may claim a winner, Pareto conclusion, yield, or efficiency uplift.",
        ),
        {"type": "table", "id": "blockers", "tableId": "blockers_table"},
        markdown(
            "next_steps",
            "## Recommended next steps\n\n"
            "1. Reconstruct each deployed V1 program binary and bind every upgrade interval to a real revision decoder.\n"
            "2. Supply the independently captured exact-cutoff terminal binding and account snapshot.\n"
            "3. Run `extract`, `audit`, and `reconcile-v1`; stop if any headline market fails exact coverage.\n"
            "4. Run `calibrate`, `evaluate`, `verify`, and `render` only after the factual replay and terminal state reconcile at zero raw-unit difference.",
        ),
        markdown(
            "questions",
            "## Open technical questions\n\n"
            "- Does the archival provider retain complete finalized history for every ephemeral upgrade buffer and Token-2022 mint?\n"
            "- Can an independent provider return every required terminal account with response context exactly equal to the cutoff slot?\n"
            "- After those gates pass, does any one global profile satisfy both safety and execution non-inferiority on the sealed holdout?",
        ),
    ]

    artifact = {
        "surface": "report",
        "manifest": {
            "version": 1,
            "surface": "report",
            "title": "Dusk versus V1 historical benchmark: implementation and audit report",
            "description": "Technical implementation status, exact benchmark contracts, verification evidence, and mandatory blockers. Contains no historical performance result.",
            "charts": charts,
            "tables": tables,
            "sources": sources,
            "blocks": blocks,
        },
        "snapshot": {"version": 1, "status": "ready", "datasets": datasets},
        "sources": sources,
    }

    write_atomic(ARTIFACT_PATH, json.dumps(artifact, indent=2, ensure_ascii=True) + "\n")
    print(ARTIFACT_PATH)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
