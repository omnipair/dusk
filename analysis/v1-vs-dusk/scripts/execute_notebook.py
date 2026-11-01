#!/usr/bin/env python3
"""Validate the sealed benchmark manifest, then execute the companion notebook.

This intentionally uses only the Python standard library.  Notebook cells get
one data object, ``metrics_manifest``, plus a small allowlist of inert builtins.
They cannot import modules or open files/network connections.
"""

from __future__ import annotations

import argparse
import ast
from contextlib import redirect_stdout
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import re
import tempfile
from types import MappingProxyType
from typing import Any, Iterable, Mapping, NoReturn, Sequence


SCHEMA_VERSION = 3
ROOT = Path(__file__).resolve().parent.parent
DEFAULT_NOTEBOOK = ROOT / "notebooks" / "benchmark.ipynb"
REQUIRED_SECTIONS = (
    "## tl;dr",
    "## Context & Methods",
    "## Data",
    "## Results",
    "## Takeaways",
)
MAX_CODE_CHARACTERS = 50_000
MAX_OUTPUT_CHARACTERS = 24_000
MAX_OUTPUT_LINES = 240
SHA256_RE = re.compile(r"[0-9a-f]{64}\Z")
UNSIGNED_RE = re.compile(r"(?:0|[1-9][0-9]*)\Z")
SIGNED_RE = re.compile(r"(?:0|[1-9][0-9]*|-[1-9][0-9]*)\Z")
LEGACY_GLOBAL_PROFILE_SEMANTICS = (
    "Legacy global_* fields and global_profile metric-ID segments identify the single "
    "protocol-wide shared Dusk profile evaluated over the explicitly bound evidence "
    "scope; they do not assert full-universe evidence."
)

TOP_LEVEL_FIELDS = (
    "schema_version",
    "run_id",
    "dataset_id",
    "profile_freeze_sha256",
    "dusk_source_sha256",
    "cutoff_slot",
    "scope",
    "common_window_start_unix_seconds",
    "common_window_end_unix_seconds_exclusive",
    "holdout_start_unix_seconds",
    "holdout_end_unix_seconds_exclusive",
    "holdout_evidence_sha256",
    "universe_window_sha256",
    "source_artifact_sha256",
    "coverage",
    "headline_hlp_scenario",
    "no_hlp_control_included",
    "global_winner_profile_id",
    "no_global_winner",
    "pareto_profile_ids",
    "per_market_calibration_qualification_sha256",
    "per_market_holdout_qualification_sha256",
    "per_market_opportunity_ceilings",
    "sensitivity_evidence",
    "markets",
    "aggregate_metrics",
    "bootstrap_intervals",
    "conservation_identities",
    "accounting_verification",
    "limitations",
    "report_metric_ids",
    "manifest_sha256",
)
SCOPE_FIELDS = (
    "scope_mode",
    "evidence_tier",
    "selected_markets",
    "selected_market_set_sha256",
    "cohort_selection_sha256",
    "cohort_representativeness",
    "final_phase_gate_sha256",
    "final_phase_gate_decision",
    "legacy_global_profile_semantics",
)
COHORT_REPRESENTATIVENESS_FIELDS = (
    "artifact_sha256",
    "cohort_selection_sha256",
    "swaps",
    "debt_actions",
    "liquidations",
)
SCOPE_ACTIVITY_COVERAGE_FIELDS = (
    "visible_universe_rows",
    "selected_panel_rows",
    "selected_panel_coverage_bps",
)
PHASE_GATE_DECISION_FIELDS = (
    "phase",
    "evidence_tier",
    "phase_eligible",
    "raw_market_eligible",
    "usd_headline_eligible",
    "raw_certification",
    "usd_certification",
    "raw_blocking_gates",
    "usd_blocking_gates",
    "limitations",
)
GATE_LIMITATION_FIELDS = ("gate", "underlying_status", "detail_code")
SCOPE_MODES = frozenset({"hybrid_panel", "full_universe"})
EVIDENCE_TIERS = frozenset({"decision_grade", "audit_grade"})
CERTIFICATION_STATUSES = frozenset({"pending", "pass", "limited", "fail"})
PERMITTED_DECISION_LIMITATIONS = frozenset(
    {"decoder_revision_certification", "terminal_state"}
)
COVERAGE_FIELDS = (
    "visible_swap_volume",
    "borrowed_notional",
    "revenue_notional",
    "exact_transaction_fetch_bps",
    "exact_decode_bps",
    "v1_zero_difference_reconciled",
)
COVERAGE_METRIC_FIELDS = (
    "availability",
    "covered_numerator",
    "total_denominator",
    "required_bps",
    "actual_bps",
    "passes",
)
MARKET_FIELDS = (
    "market",
    "token0",
    "token1",
    "has_lending_activity",
    "raw_metrics",
    "usd_metrics",
)
NAMED_METRIC_FIELDS = (
    "metric_id",
    "family",
    "profile_id",
    "replay_arm",
    "hlp_scenario",
    "definition",
    "revenue_dimensions",
    "v1",
    "dusk",
    "paired_delta",
)
REVENUE_DIMENSION_FIELDS = (
    "lane",
    "accounting_class",
    "origin",
    "v1_venue",
    "dusk_venue",
)
METRIC_VALUE_FIELDS = (
    "availability",
    "numerator",
    "denominator",
    "unit",
    "denominator_definition",
)
BOOTSTRAP_FIELDS = (
    "metric_id",
    "block_days",
    "samples",
    "lower_025",
    "median",
    "upper_975",
    "unit",
)
CONSERVATION_FIELDS = (
    "identity_id",
    "initial",
    "external_credits",
    "external_debits",
    "final_balance",
)
ACCOUNTING_FIELDS = (
    "v1_conservation_checks",
    "v1_conservation_failures",
    "dusk_atomic_transition_checks",
    "dusk_atomic_transition_failures",
    "paired_revenue_lane_checks",
    "paired_revenue_lane_failures",
)
OPPORTUNITY_CEILING_FIELDS = (
    "market",
    "training_swap_count",
    "training_active_days",
    "validation_swap_count",
    "validation_active_days",
    "holdout_swap_count",
    "holdout_active_days",
    "selected_profile_id",
    "claim_available",
    "unavailable_reason",
)
SENSITIVITY_SUMMARY_FIELDS = (
    "profile_id",
    "replay_arm",
    "hlp_scenario",
    "sensitivity_artifact_sha256",
    "included_markets_sha256",
    "sampled_block_sequence_sha256",
    "bootstrap_samples",
    "top_market_rankings",
    "top_market_exclusions",
    "top_market_exclusion_unsupported_reason",
)
SENSITIVITY_RATIONAL_FIELDS = ("numerator", "denominator")
EXACT_AND_REPORTED_FIELDS = ("exact", "reported_signed_integer")
RANKED_MARKET_FIELDS = ("rank", "market", "factual_value_usd_micro")
TOP_MARKET_RANKING_FIELDS = ("basis", "markets")
TOP_MARKET_EXCLUSION_FIELDS = (
    "basis",
    "excluded_market",
    "excluded_factual_value_usd_micro",
    "remaining_market_count",
    "estimate_without_top_market",
)
SENSITIVITY_POINT_FIELDS = (
    "stakeholder_revenue_v1_usd_micro",
    "stakeholder_revenue_dusk_user_flow_usd_micro",
    "stakeholder_user_flow_revenue_delta_usd_micro",
    "dusk_synthetic_arbitrage_revenue_usd_micro",
    "dusk_synthetic_protocol_settlement_revenue_usd_micro",
    "factual_swap_input_usd_micro",
    "quoted_execution_cost_delta_ppb",
    "quoted_execution_cost_unsupported_reason",
    "unsupported_execution_rows",
)
TOP_MARKET_BASES = frozenset(
    {"factual_usd_swap_input", "factual_stakeholder_revenue_usd"}
)

AVAILABILITIES = frozenset(
    {
        "available",
        "not_applicable_no_lending_activity",
        "not_applicable_venue_mechanism",
        "missing_historical_usd_mark",
        "window_too_short_for_annualization",
        "missing_ownership_denominator",
        "missing_cash_realization_evidence",
        "outside_common_window",
        "failed_coverage_gate",
        "not_evaluated",
    }
)
FAMILIES = frozenset(
    {
        "collateral_efficiency",
        "spot_capital_efficiency",
        "leverage",
        "lp_revenue",
        "protocol_revenue",
        "balance_sheet",
        "safety",
        "coverage",
    }
)
REPLAY_ARMS = frozenset({"fixed_flow", "intent"})
HLP_SCENARIOS = frozenset({"no_hlp", "maximum_feasible"})
REVENUE_ORIGINS = frozenset(
    {"factual_user_flow", "synthetic_arbitrage", "synthetic_protocol_settlement"}
)
REVENUE_VENUES = frozenset({"v1", "dusk"})


class ManifestValidationError(ValueError):
    """The result manifest is not a trustworthy ResultManifestV1 artifact."""


class NotebookValidationError(ValueError):
    """The source notebook violates its reproducibility/sandbox contract."""


class NotebookExecutionError(RuntimeError):
    """A validated notebook cell did not execute successfully."""


def _fail(message: str) -> NoReturn:
    raise ManifestValidationError(message)


def _duplicate_checked_object(pairs: Sequence[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ManifestValidationError(f"duplicate JSON key: {key!r}")
        result[key] = value
    return result


def _reject_json_constant(value: str) -> NoReturn:
    raise ManifestValidationError(f"non-finite JSON number is forbidden: {value}")


def loads_json_strict(payload: str, *, source: str) -> Any:
    try:
        return json.loads(
            payload,
            object_pairs_hook=_duplicate_checked_object,
            parse_constant=_reject_json_constant,
        )
    except ManifestValidationError:
        raise
    except (json.JSONDecodeError, UnicodeError) as exc:
        raise ManifestValidationError(f"invalid JSON in {source}: {exc}") from exc


def load_json_strict(path: Path) -> Any:
    try:
        payload = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise ManifestValidationError(f"cannot read {path}: {exc}") from exc
    return loads_json_strict(payload, source=str(path))


def _object(value: Any, where: str, fields: Sequence[str]) -> Mapping[str, Any]:
    if type(value) is not dict:
        _fail(f"{where} must be an object")
    expected = set(fields)
    actual = set(value)
    missing = sorted(expected - actual)
    unknown = sorted(actual - expected)
    if missing or unknown:
        _fail(f"{where} field mismatch; missing={missing}, unknown={unknown}")
    return value


def _list(value: Any, where: str) -> list[Any]:
    if type(value) is not list:
        _fail(f"{where} must be an array")
    return value


def _string(value: Any, where: str, *, allow_empty: bool = False) -> str:
    if type(value) is not str or (not allow_empty and not value):
        _fail(f"{where} must be {'a' if allow_empty else 'a non-empty'} string")
    return value


def _bool(value: Any, where: str) -> bool:
    if type(value) is not bool:
        _fail(f"{where} must be a boolean")
    return value


def _integer(value: Any, where: str, *, minimum: int | None = None) -> int:
    if type(value) is not int:
        _fail(f"{where} must be an integer")
    if minimum is not None and value < minimum:
        _fail(f"{where} must be at least {minimum}")
    return value


def _sha256(value: Any, where: str) -> str:
    text = _string(value, where)
    if SHA256_RE.fullmatch(text) is None:
        _fail(f"{where} must be a lowercase 64-character SHA-256")
    return text


def _unsigned_string(value: Any, where: str, *, positive: bool = False) -> str:
    text = _string(value, where)
    if UNSIGNED_RE.fullmatch(text) is None or (positive and text == "0"):
        qualifier = "positive canonical" if positive else "canonical unsigned"
        _fail(f"{where} must be a {qualifier} decimal integer string")
    return text


def _signed_string(value: Any, where: str) -> str:
    text = _string(value, where)
    if SIGNED_RE.fullmatch(text) is None:
        _fail(f"{where} must be a canonical signed decimal integer string")
    return text


def _optional_string(
    value: Any, where: str, *, allowed: frozenset[str] | None = None
) -> str | None:
    if value is None:
        return None
    text = _string(value, where)
    if allowed is not None and text not in allowed:
        _fail(f"{where} has unsupported value {text!r}")
    return text


def _string_array(value: Any, where: str, *, unique: bool = False) -> list[str]:
    items = _list(value, where)
    output = [_string(item, f"{where}[{index}]") for index, item in enumerate(items)]
    if unique and len(set(output)) != len(output):
        _fail(f"{where} contains duplicate values")
    return output


def _market_set_sha256(markets: Sequence[str]) -> str:
    digest = hashlib.sha256()
    digest.update(b"v1-vs-dusk/included-markets/v1\0")
    for market in markets:
        encoded = market.encode("utf-8")
        digest.update(len(encoded).to_bytes(8, "little"))
        digest.update(encoded)
    return digest.hexdigest()


def _validate_scope_activity_coverage(value: Any, where: str) -> None:
    coverage = _object(value, where, SCOPE_ACTIVITY_COVERAGE_FIELDS)
    visible = _integer(
        coverage["visible_universe_rows"], f"{where}.visible_universe_rows", minimum=0
    )
    selected = _integer(
        coverage["selected_panel_rows"], f"{where}.selected_panel_rows", minimum=0
    )
    bps = _integer(
        coverage["selected_panel_coverage_bps"],
        f"{where}.selected_panel_coverage_bps",
        minimum=0,
    )
    expected_bps = 0 if visible == 0 else selected * 10_000 // visible
    if selected > visible or bps > 10_000 or bps != expected_bps:
        _fail(f"{where} selected-panel coverage is inconsistent")


def _validate_final_phase_gate_decision(
    value: Any, where: str, evidence_tier: str
) -> None:
    decision = _object(value, where, PHASE_GATE_DECISION_FIELDS)
    if _string(decision["phase"], f"{where}.phase") != "post_replay_final":
        _fail(f"{where}.phase must be post_replay_final")
    if _string(decision["evidence_tier"], f"{where}.evidence_tier") != evidence_tier:
        _fail(f"{where}.evidence_tier disagrees with scope")
    for name in ("phase_eligible", "raw_market_eligible", "usd_headline_eligible"):
        if not _bool(decision[name], f"{where}.{name}"):
            _fail(f"{where}.{name} must pass")
    for name in ("raw_blocking_gates", "usd_blocking_gates"):
        if _string_array(decision[name], f"{where}.{name}", unique=True):
            _fail(f"{where}.{name} must be empty")

    raw_certification = _string(
        decision["raw_certification"], f"{where}.raw_certification"
    )
    usd_certification = _string(
        decision["usd_certification"], f"{where}.usd_certification"
    )
    if (
        raw_certification not in CERTIFICATION_STATUSES
        or usd_certification not in CERTIFICATION_STATUSES
    ):
        _fail(f"{where} has an unsupported certification status")

    limited_gates: list[str] = []
    for index, value_limitation in enumerate(
        _list(decision["limitations"], f"{where}.limitations")
    ):
        limitation_where = f"{where}.limitations[{index}]"
        limitation = _object(
            value_limitation, limitation_where, GATE_LIMITATION_FIELDS
        )
        gate = _string(limitation["gate"], f"{limitation_where}.gate")
        if gate not in PERMITTED_DECISION_LIMITATIONS:
            _fail(f"{limitation_where}.gate is not a permitted typed limitation")
        if (
            _string(
                limitation["underlying_status"],
                f"{limitation_where}.underlying_status",
            )
            != "fail"
        ):
            _fail(f"{limitation_where}.underlying_status must be fail")
        _string(limitation["detail_code"], f"{limitation_where}.detail_code")
        limited_gates.append(gate)
    if len(limited_gates) != len(set(limited_gates)):
        _fail(f"{where}.limitations contains duplicate gates")

    if evidence_tier == "audit_grade":
        if limited_gates or raw_certification != "pass" or usd_certification != "pass":
            _fail("AuditGrade evidence requires both certifications to pass")
    elif limited_gates:
        if raw_certification != "limited" or usd_certification != "limited":
            _fail("DecisionGrade limitations require limited certification status")
    elif raw_certification != "pass" or usd_certification != "pass":
        _fail("DecisionGrade evidence without limitations must pass certification")


def _source_hash_for_suffix(
    source_hashes: Mapping[str, Any], suffix: str, where: str
) -> str:
    matches = [
        digest
        for name, digest in source_hashes.items()
        if name == suffix or name.endswith(f"/{suffix}")
    ]
    if len(matches) != 1:
        _fail(f"{where} requires exactly one source artifact ending in {suffix!r}")
    return _sha256(matches[0], f"{where} source hash")


def _validate_result_scope(
    value: Any, source_hashes: Mapping[str, Any]
) -> dict[str, Any]:
    scope = dict(_object(value, "scope", SCOPE_FIELDS))
    scope_mode = _string(scope["scope_mode"], "scope.scope_mode")
    evidence_tier = _string(scope["evidence_tier"], "scope.evidence_tier")
    if scope_mode not in SCOPE_MODES or evidence_tier not in EVIDENCE_TIERS:
        _fail("scope has an unsupported mode or evidence tier")
    if scope_mode == "full_universe" and evidence_tier != "audit_grade":
        _fail("full_universe scope requires AuditGrade evidence")

    selected_markets = _string_array(
        scope["selected_markets"], "scope.selected_markets", unique=True
    )
    if not selected_markets or selected_markets != sorted(selected_markets):
        _fail("scope.selected_markets must be nonempty, sorted, and unique")
    selected_hash = _sha256(
        scope["selected_market_set_sha256"], "scope.selected_market_set_sha256"
    )
    if selected_hash != _market_set_sha256(selected_markets):
        _fail("scope.selected_market_set_sha256 does not match selected_markets")

    cohort_sha256 = scope["cohort_selection_sha256"]
    representativeness = scope["cohort_representativeness"]
    if scope_mode == "hybrid_panel":
        cohort_sha256 = _sha256(cohort_sha256, "scope.cohort_selection_sha256")
        cohort = _object(
            representativeness,
            "scope.cohort_representativeness",
            COHORT_REPRESENTATIVENESS_FIELDS,
        )
        _sha256(
            cohort["artifact_sha256"],
            "scope.cohort_representativeness.artifact_sha256",
        )
        if (
            _sha256(
                cohort["cohort_selection_sha256"],
                "scope.cohort_representativeness.cohort_selection_sha256",
            )
            != cohort_sha256
        ):
            _fail("cohort representativeness binds another cohort selection")
        for name in ("swaps", "debt_actions", "liquidations"):
            _validate_scope_activity_coverage(
                cohort[name], f"scope.cohort_representativeness.{name}"
            )
        _source_hash_for_suffix(
            source_hashes, "cohort-selection.json", "scope.cohort_selection_sha256"
        )
        _source_hash_for_suffix(
            source_hashes,
            "cohort-representativeness.json",
            "scope.cohort_representativeness",
        )
    elif cohort_sha256 is not None or representativeness is not None:
        _fail("full_universe scope cannot carry selected-panel cohort evidence")

    final_gate_sha256 = _sha256(
        scope["final_phase_gate_sha256"], "scope.final_phase_gate_sha256"
    )
    if final_gate_sha256 != _source_hash_for_suffix(
        source_hashes, "v1-final-phase-gate.json", "scope.final_phase_gate_sha256"
    ):
        _fail("scope.final_phase_gate_sha256 disagrees with source artifacts")
    _validate_final_phase_gate_decision(
        scope["final_phase_gate_decision"],
        "scope.final_phase_gate_decision",
        evidence_tier,
    )
    if scope["legacy_global_profile_semantics"] != LEGACY_GLOBAL_PROFILE_SEMANTICS:
        _fail("scope.legacy_global_profile_semantics has changed")
    return scope


def _validate_metric_value(value: Any, where: str) -> None:
    metric = _object(value, where, METRIC_VALUE_FIELDS)
    availability = _string(metric["availability"], f"{where}.availability")
    if availability not in AVAILABILITIES:
        _fail(f"{where}.availability has unsupported value {availability!r}")
    _string(metric["unit"], f"{where}.unit")
    definition = metric["denominator_definition"]
    if definition is not None:
        _string(definition, f"{where}.denominator_definition")
    numerator = metric["numerator"]
    denominator = metric["denominator"]
    if availability == "available":
        _signed_string(numerator, f"{where}.numerator")
        _unsigned_string(denominator, f"{where}.denominator", positive=True)
    elif numerator is not None or denominator is not None:
        _fail(
            f"{where} is unavailable but still contains numerator/denominator data"
        )


def _validate_named_metric(value: Any, where: str) -> str:
    metric = _object(value, where, NAMED_METRIC_FIELDS)
    metric_id = _string(metric["metric_id"], f"{where}.metric_id")
    family = _string(metric["family"], f"{where}.family")
    if family not in FAMILIES:
        _fail(f"{where}.family has unsupported value {family!r}")
    _optional_string(metric["profile_id"], f"{where}.profile_id")
    _optional_string(metric["replay_arm"], f"{where}.replay_arm", allowed=REPLAY_ARMS)
    _optional_string(
        metric["hlp_scenario"], f"{where}.hlp_scenario", allowed=HLP_SCENARIOS
    )
    _string(metric["definition"], f"{where}.definition")
    dimensions = metric["revenue_dimensions"]
    if dimensions is not None:
        dimensions = _object(
            dimensions, f"{where}.revenue_dimensions", REVENUE_DIMENSION_FIELDS
        )
        _string(dimensions["lane"], f"{where}.revenue_dimensions.lane")
        _string(
            dimensions["accounting_class"],
            f"{where}.revenue_dimensions.accounting_class",
        )
        origin = _string(
            dimensions["origin"], f"{where}.revenue_dimensions.origin"
        )
        if origin not in REVENUE_ORIGINS:
            _fail(f"{where}.revenue_dimensions.origin is unsupported")
        v1_venue = _optional_string(
            dimensions["v1_venue"], f"{where}.revenue_dimensions.v1_venue"
        )
        dusk_venue = _optional_string(
            dimensions["dusk_venue"], f"{where}.revenue_dimensions.dusk_venue"
        )
        if v1_venue not in (None, "v1") or dusk_venue not in (None, "dusk"):
            _fail(f"{where}.revenue_dimensions has a wrong venue identifier")
        if v1_venue is None and dusk_venue is None:
            _fail(f"{where}.revenue_dimensions has no venue")
        if origin != "factual_user_flow" and v1_venue is not None:
            _fail(f"{where}.revenue_dimensions maps synthetic revenue to V1")
    _validate_metric_value(metric["v1"], f"{where}.v1")
    _validate_metric_value(metric["dusk"], f"{where}.dusk")
    _validate_metric_value(metric["paired_delta"], f"{where}.paired_delta")
    return metric_id


def _validate_coverage_metric(value: Any, where: str) -> None:
    coverage = _object(value, where, COVERAGE_METRIC_FIELDS)
    availability = _string(coverage["availability"], f"{where}.availability")
    covered = int(_unsigned_string(coverage["covered_numerator"], f"{where}.covered_numerator"))
    total = int(_unsigned_string(coverage["total_denominator"], f"{where}.total_denominator"))
    required = _integer(coverage["required_bps"], f"{where}.required_bps", minimum=0)
    if required > 10_000 or covered > total:
        _fail(f"{where} has invalid numerator, denominator, or threshold")
    passes = _bool(coverage["passes"], f"{where}.passes")
    actual = coverage["actual_bps"]
    if availability == "available":
        if total == 0:
            _fail(f"{where} cannot be available with zero denominator")
        actual_bps = _integer(actual, f"{where}.actual_bps", minimum=0)
        expected = covered * 10_000 // total
        if actual_bps != expected or passes != (expected >= required):
            _fail(f"{where} basis points or pass state is inconsistent")
    elif availability == "not_applicable_no_lending_activity":
        if covered != 0 or total != 0 or actual is not None or not passes:
            _fail(f"{where} no-activity coverage state is inconsistent")
    else:
        _fail(f"{where} has unsupported coverage availability {availability!r}")
    if not passes:
        _fail(f"{where} fails the frozen headline coverage gate")


def _validate_coverage(value: Any) -> None:
    coverage = _object(value, "coverage", COVERAGE_FIELDS)
    for name in ("visible_swap_volume", "borrowed_notional", "revenue_notional"):
        _validate_coverage_metric(coverage[name], f"coverage.{name}")
    if _integer(coverage["exact_transaction_fetch_bps"], "coverage.exact_transaction_fetch_bps") != 10_000:
        _fail("coverage.exact_transaction_fetch_bps must equal 10000")
    if _integer(coverage["exact_decode_bps"], "coverage.exact_decode_bps") != 10_000:
        _fail("coverage.exact_decode_bps must equal 10000")
    if not _bool(
        coverage["v1_zero_difference_reconciled"],
        "coverage.v1_zero_difference_reconciled",
    ):
        _fail("coverage requires zero-difference V1 reconciliation")


def _validate_bootstrap(value: Any, where: str) -> None:
    interval = _object(value, where, BOOTSTRAP_FIELDS)
    _string(interval["metric_id"], f"{where}.metric_id")
    _integer(interval["block_days"], f"{where}.block_days", minimum=1)
    _integer(interval["samples"], f"{where}.samples", minimum=1)
    lower = int(_signed_string(interval["lower_025"], f"{where}.lower_025"))
    median = int(_signed_string(interval["median"], f"{where}.median"))
    upper = int(_signed_string(interval["upper_975"], f"{where}.upper_975"))
    if not lower <= median <= upper:
        _fail(f"{where} quantiles are not ordered")
    _string(interval["unit"], f"{where}.unit")


def _validate_conservation(value: Any, where: str) -> str:
    identity = _object(value, where, CONSERVATION_FIELDS)
    identity_id = _string(identity["identity_id"], f"{where}.identity_id")
    initial = int(_unsigned_string(identity["initial"], f"{where}.initial"))
    credits = int(_unsigned_string(identity["external_credits"], f"{where}.external_credits"))
    debits = int(_unsigned_string(identity["external_debits"], f"{where}.external_debits"))
    final_balance = int(_unsigned_string(identity["final_balance"], f"{where}.final_balance"))
    if initial + credits != debits + final_balance:
        _fail(f"{where} does not conserve value")
    return identity_id


def _validate_accounting(value: Any) -> None:
    accounting = _object(value, "accounting_verification", ACCOUNTING_FIELDS)
    for field in ACCOUNTING_FIELDS:
        _integer(accounting[field], f"accounting_verification.{field}", minimum=0)
    if (
        accounting["v1_conservation_checks"] == 0
        or accounting["dusk_atomic_transition_checks"] == 0
        or accounting["paired_revenue_lane_checks"] == 0
        or accounting["v1_conservation_failures"] != 0
        or accounting["dusk_atomic_transition_failures"] != 0
        or accounting["paired_revenue_lane_failures"] != 0
    ):
        _fail("accounting verification is incomplete or contains failures")


def _validate_opportunity_ceiling(value: Any, where: str) -> str:
    ceiling = _object(value, where, OPPORTUNITY_CEILING_FIELDS)
    market = _string(ceiling["market"], f"{where}.market")
    for field in (
        "training_swap_count",
        "training_active_days",
        "validation_swap_count",
        "validation_active_days",
        "holdout_swap_count",
        "holdout_active_days",
    ):
        _integer(ceiling[field], f"{where}.{field}", minimum=0)
    selected = _optional_string(
        ceiling["selected_profile_id"], f"{where}.selected_profile_id"
    )
    claim_available = _bool(
        ceiling["claim_available"], f"{where}.claim_available"
    )
    unavailable = _optional_string(
        ceiling["unavailable_reason"], f"{where}.unavailable_reason"
    )
    if claim_available != (selected is not None) or claim_available == (
        unavailable is not None
    ):
        _fail(f"{where} has an inconsistent opportunity-claim state")
    return market


def _validate_sensitivity_rational(value: Any, where: str) -> None:
    rational = _object(value, where, SENSITIVITY_RATIONAL_FIELDS)
    _signed_string(rational["numerator"], f"{where}.numerator")
    _unsigned_string(rational["denominator"], f"{where}.denominator", positive=True)


def _validate_exact_and_reported(value: Any, where: str) -> None:
    exact = _object(value, where, EXACT_AND_REPORTED_FIELDS)
    _validate_sensitivity_rational(exact["exact"], f"{where}.exact")
    _signed_string(exact["reported_signed_integer"], f"{where}.reported_signed_integer")


def _validate_sensitivity_point(value: Any, where: str) -> None:
    point = _object(value, where, SENSITIVITY_POINT_FIELDS)
    for name in (
        "stakeholder_revenue_v1_usd_micro",
        "stakeholder_revenue_dusk_user_flow_usd_micro",
        "dusk_synthetic_arbitrage_revenue_usd_micro",
        "dusk_synthetic_protocol_settlement_revenue_usd_micro",
        "factual_swap_input_usd_micro",
    ):
        _validate_sensitivity_rational(point[name], f"{where}.{name}")
    _validate_exact_and_reported(
        point["stakeholder_user_flow_revenue_delta_usd_micro"],
        f"{where}.stakeholder_user_flow_revenue_delta_usd_micro",
    )
    quoted = point["quoted_execution_cost_delta_ppb"]
    reason = point["quoted_execution_cost_unsupported_reason"]
    if quoted is None:
        _string(reason, f"{where}.quoted_execution_cost_unsupported_reason")
    else:
        _validate_exact_and_reported(quoted, f"{where}.quoted_execution_cost_delta_ppb")
        if reason is not None:
            _fail(f"{where} supplies both an execution estimate and unsupported reason")
    _integer(point["unsupported_execution_rows"], f"{where}.unsupported_execution_rows", minimum=0)


def _validate_top_market_ranking(value: Any, where: str) -> str:
    ranking = _object(value, where, TOP_MARKET_RANKING_FIELDS)
    basis = _string(ranking["basis"], f"{where}.basis")
    if basis not in TOP_MARKET_BASES:
        _fail(f"{where}.basis is unsupported")
    ranks: list[int] = []
    markets: list[str] = []
    for index, value_market in enumerate(_list(ranking["markets"], f"{where}.markets")):
        ranked = _object(value_market, f"{where}.markets[{index}]", RANKED_MARKET_FIELDS)
        ranks.append(_integer(ranked["rank"], f"{where}.markets[{index}].rank", minimum=1))
        markets.append(_string(ranked["market"], f"{where}.markets[{index}].market"))
        _validate_sensitivity_rational(
            ranked["factual_value_usd_micro"],
            f"{where}.markets[{index}].factual_value_usd_micro",
        )
    if ranks != list(range(1, len(ranks) + 1)) or len(markets) != len(set(markets)):
        _fail(f"{where} has non-contiguous ranks or duplicate markets")
    return basis


def _validate_top_market_exclusion(value: Any, where: str) -> str:
    exclusion = _object(value, where, TOP_MARKET_EXCLUSION_FIELDS)
    basis = _string(exclusion["basis"], f"{where}.basis")
    if basis not in TOP_MARKET_BASES:
        _fail(f"{where}.basis is unsupported")
    _string(exclusion["excluded_market"], f"{where}.excluded_market")
    _validate_sensitivity_rational(
        exclusion["excluded_factual_value_usd_micro"],
        f"{where}.excluded_factual_value_usd_micro",
    )
    _integer(exclusion["remaining_market_count"], f"{where}.remaining_market_count", minimum=1)
    _validate_sensitivity_point(
        exclusion["estimate_without_top_market"],
        f"{where}.estimate_without_top_market",
    )
    return basis


def _validate_sensitivity_summary(value: Any, where: str) -> str:
    evidence = _object(value, where, SENSITIVITY_SUMMARY_FIELDS)
    profile = _string(evidence["profile_id"], f"{where}.profile_id")
    arm = _string(evidence["replay_arm"], f"{where}.replay_arm")
    if arm not in REPLAY_ARMS:
        _fail(f"{where}.replay_arm is unsupported")
    scenario = _string(evidence["hlp_scenario"], f"{where}.hlp_scenario")
    if scenario not in HLP_SCENARIOS:
        _fail(f"{where}.hlp_scenario is unsupported")
    for name in (
        "sensitivity_artifact_sha256",
        "included_markets_sha256",
        "sampled_block_sequence_sha256",
    ):
        _sha256(evidence[name], f"{where}.{name}")
    _integer(evidence["bootstrap_samples"], f"{where}.bootstrap_samples", minimum=1)
    ranking_bases = [
        _validate_top_market_ranking(item, f"{where}.top_market_rankings[{index}]")
        for index, item in enumerate(
            _list(evidence["top_market_rankings"], f"{where}.top_market_rankings")
        )
    ]
    exclusion_bases = [
        _validate_top_market_exclusion(item, f"{where}.top_market_exclusions[{index}]")
        for index, item in enumerate(
            _list(evidence["top_market_exclusions"], f"{where}.top_market_exclusions")
        )
    ]
    if len(ranking_bases) != len(set(ranking_bases)) or len(exclusion_bases) != len(set(exclusion_bases)):
        _fail(f"{where} contains duplicate ranking or exclusion bases")
    reason = evidence["top_market_exclusion_unsupported_reason"]
    if exclusion_bases:
        if reason is not None or set(exclusion_bases) != set(ranking_bases):
            _fail(f"{where} has inconsistent top-market exclusion evidence")
    else:
        _string(reason, f"{where}.top_market_exclusion_unsupported_reason")
    return f"{profile}:{arm}:{scenario}"


def _ordered(value: Mapping[str, Any], fields: Sequence[str]) -> dict[str, Any]:
    return {field: value[field] for field in fields}


def _canonical_metric_value(value: Mapping[str, Any]) -> dict[str, Any]:
    return _ordered(value, METRIC_VALUE_FIELDS)


def _canonical_named_metric(value: Mapping[str, Any]) -> dict[str, Any]:
    output = _ordered(value, NAMED_METRIC_FIELDS)
    for name in ("v1", "dusk", "paired_delta"):
        output[name] = _canonical_metric_value(value[name])
    return output


def _canonical_coverage_metric(value: Mapping[str, Any]) -> dict[str, Any]:
    return _ordered(value, COVERAGE_METRIC_FIELDS)


def _canonical_gate_decision(value: Mapping[str, Any]) -> dict[str, Any]:
    output = _ordered(value, PHASE_GATE_DECISION_FIELDS)
    output["limitations"] = [
        _ordered(limitation, GATE_LIMITATION_FIELDS)
        for limitation in value["limitations"]
    ]
    return output


def _canonical_result_scope(value: Mapping[str, Any]) -> dict[str, Any]:
    output = _ordered(value, SCOPE_FIELDS)
    representativeness = value["cohort_representativeness"]
    if representativeness is not None:
        canonical_representativeness = _ordered(
            representativeness, COHORT_REPRESENTATIVENESS_FIELDS
        )
        for name in ("swaps", "debt_actions", "liquidations"):
            canonical_representativeness[name] = _ordered(
                representativeness[name], SCOPE_ACTIVITY_COVERAGE_FIELDS
            )
        output["cohort_representativeness"] = canonical_representativeness
    output["final_phase_gate_decision"] = _canonical_gate_decision(
        value["final_phase_gate_decision"]
    )
    return output


def _canonical_sensitivity_rational(value: Mapping[str, Any]) -> dict[str, Any]:
    return _ordered(value, SENSITIVITY_RATIONAL_FIELDS)


def _canonical_exact_and_reported(value: Mapping[str, Any]) -> dict[str, Any]:
    output = _ordered(value, EXACT_AND_REPORTED_FIELDS)
    output["exact"] = _canonical_sensitivity_rational(value["exact"])
    return output


def _canonical_sensitivity_point(value: Mapping[str, Any]) -> dict[str, Any]:
    output = _ordered(value, SENSITIVITY_POINT_FIELDS)
    for name in (
        "stakeholder_revenue_v1_usd_micro",
        "stakeholder_revenue_dusk_user_flow_usd_micro",
        "dusk_synthetic_arbitrage_revenue_usd_micro",
        "dusk_synthetic_protocol_settlement_revenue_usd_micro",
        "factual_swap_input_usd_micro",
    ):
        output[name] = _canonical_sensitivity_rational(value[name])
    output["stakeholder_user_flow_revenue_delta_usd_micro"] = (
        _canonical_exact_and_reported(
            value["stakeholder_user_flow_revenue_delta_usd_micro"]
        )
    )
    if value["quoted_execution_cost_delta_ppb"] is not None:
        output["quoted_execution_cost_delta_ppb"] = _canonical_exact_and_reported(
            value["quoted_execution_cost_delta_ppb"]
        )
    return output


def _canonical_top_market_ranking(value: Mapping[str, Any]) -> dict[str, Any]:
    output = _ordered(value, TOP_MARKET_RANKING_FIELDS)
    output["markets"] = []
    for ranked in value["markets"]:
        canonical = _ordered(ranked, RANKED_MARKET_FIELDS)
        canonical["factual_value_usd_micro"] = _canonical_sensitivity_rational(
            ranked["factual_value_usd_micro"]
        )
        output["markets"].append(canonical)
    return output


def _canonical_top_market_exclusion(value: Mapping[str, Any]) -> dict[str, Any]:
    output = _ordered(value, TOP_MARKET_EXCLUSION_FIELDS)
    output["excluded_factual_value_usd_micro"] = _canonical_sensitivity_rational(
        value["excluded_factual_value_usd_micro"]
    )
    output["estimate_without_top_market"] = _canonical_sensitivity_point(
        value["estimate_without_top_market"]
    )
    return output


def _canonical_sensitivity_summary(value: Mapping[str, Any]) -> dict[str, Any]:
    output = _ordered(value, SENSITIVITY_SUMMARY_FIELDS)
    output["top_market_rankings"] = [
        _canonical_top_market_ranking(ranking)
        for ranking in value["top_market_rankings"]
    ]
    output["top_market_exclusions"] = [
        _canonical_top_market_exclusion(exclusion)
        for exclusion in value["top_market_exclusions"]
    ]
    return output


def _canonical_manifest(manifest: Mapping[str, Any]) -> dict[str, Any]:
    """Reproduce serde's struct/BTreeMap/BTreeSet serialization order."""
    output = _ordered(manifest, TOP_LEVEL_FIELDS)
    output["scope"] = _canonical_result_scope(manifest["scope"])
    coverage = _ordered(manifest["coverage"], COVERAGE_FIELDS)
    for name in ("visible_swap_volume", "borrowed_notional", "revenue_notional"):
        coverage[name] = _canonical_coverage_metric(manifest["coverage"][name])
    output["coverage"] = coverage
    output["source_artifact_sha256"] = {
        key: manifest["source_artifact_sha256"][key]
        for key in sorted(manifest["source_artifact_sha256"])
    }
    output["per_market_opportunity_ceilings"] = [
        _ordered(ceiling, OPPORTUNITY_CEILING_FIELDS)
        for ceiling in manifest["per_market_opportunity_ceilings"]
    ]
    output["sensitivity_evidence"] = [
        _canonical_sensitivity_summary(evidence)
        for evidence in manifest["sensitivity_evidence"]
    ]
    markets = []
    for market in manifest["markets"]:
        canonical_market = _ordered(market, MARKET_FIELDS)
        canonical_market["raw_metrics"] = [
            _canonical_named_metric(metric) for metric in market["raw_metrics"]
        ]
        canonical_market["usd_metrics"] = [
            _canonical_named_metric(metric) for metric in market["usd_metrics"]
        ]
        markets.append(canonical_market)
    output["markets"] = markets
    output["aggregate_metrics"] = [
        _canonical_named_metric(metric) for metric in manifest["aggregate_metrics"]
    ]
    output["bootstrap_intervals"] = [
        _ordered(interval, BOOTSTRAP_FIELDS)
        for interval in manifest["bootstrap_intervals"]
    ]
    output["conservation_identities"] = [
        _ordered(identity, CONSERVATION_FIELDS)
        for identity in manifest["conservation_identities"]
    ]
    output["accounting_verification"] = _ordered(
        manifest["accounting_verification"], ACCOUNTING_FIELDS
    )
    output["report_metric_ids"] = sorted(manifest["report_metric_ids"])
    return output


def compute_manifest_sha256(manifest: Mapping[str, Any]) -> str:
    unsigned = _canonical_manifest(manifest)
    unsigned["manifest_sha256"] = ""
    payload = json.dumps(
        unsigned,
        ensure_ascii=False,
        allow_nan=False,
        separators=(",", ":"),
    ).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def validate_manifest(value: Any) -> dict[str, Any]:
    manifest = dict(_object(value, "manifest", TOP_LEVEL_FIELDS))
    if _integer(manifest["schema_version"], "schema_version") != SCHEMA_VERSION:
        _fail(f"unsupported metrics schema version {manifest['schema_version']!r}")
    _string(manifest["run_id"], "run_id")
    _sha256(manifest["dataset_id"], "dataset_id")
    for field in (
        "profile_freeze_sha256",
        "dusk_source_sha256",
        "holdout_evidence_sha256",
        "universe_window_sha256",
        "per_market_calibration_qualification_sha256",
        "per_market_holdout_qualification_sha256",
        "manifest_sha256",
    ):
        _sha256(manifest[field], field)
    _integer(manifest["cutoff_slot"], "cutoff_slot", minimum=0)
    common_start = _integer(
        manifest["common_window_start_unix_seconds"],
        "common_window_start_unix_seconds",
    )
    common_end = _integer(
        manifest["common_window_end_unix_seconds_exclusive"],
        "common_window_end_unix_seconds_exclusive",
    )
    holdout_start = _integer(
        manifest["holdout_start_unix_seconds"], "holdout_start_unix_seconds"
    )
    holdout_end = _integer(
        manifest["holdout_end_unix_seconds_exclusive"],
        "holdout_end_unix_seconds_exclusive",
    )
    if not common_start <= holdout_start < holdout_end <= common_end or common_start >= common_end:
        _fail("common/holdout windows are invalid or not nested")

    source_hashes = manifest["source_artifact_sha256"]
    if type(source_hashes) is not dict or not source_hashes:
        _fail("source_artifact_sha256 must be a non-empty object")
    for key, digest in source_hashes.items():
        _string(key, "source_artifact_sha256 key")
        _sha256(digest, f"source_artifact_sha256[{key!r}]")
    scope = _validate_result_scope(manifest["scope"], source_hashes)

    _validate_coverage(manifest["coverage"])
    headline_hlp_scenario = _string(
        manifest["headline_hlp_scenario"], "headline_hlp_scenario"
    )
    if headline_hlp_scenario != "maximum_feasible":
        _fail("headline_hlp_scenario must be maximum_feasible")
    _bool(manifest["no_hlp_control_included"], "no_hlp_control_included")
    winner = _optional_string(manifest["global_winner_profile_id"], "global_winner_profile_id")
    no_winner = _bool(manifest["no_global_winner"], "no_global_winner")
    if no_winner == (winner is not None):
        _fail("global winner and no-winner flags are inconsistent")
    _string_array(manifest["pareto_profile_ids"], "pareto_profile_ids", unique=True)

    opportunity_markets = [
        _validate_opportunity_ceiling(ceiling, f"per_market_opportunity_ceilings[{index}]")
        for index, ceiling in enumerate(
            _list(
                manifest["per_market_opportunity_ceilings"],
                "per_market_opportunity_ceilings",
            )
        )
    ]
    if len(set(opportunity_markets)) != len(opportunity_markets):
        _fail("per_market_opportunity_ceilings contains duplicate markets")

    sensitivity_identities = [
        _validate_sensitivity_summary(evidence, f"sensitivity_evidence[{index}]")
        for index, evidence in enumerate(
            _list(manifest["sensitivity_evidence"], "sensitivity_evidence")
        )
    ]
    if len(set(sensitivity_identities)) != len(sensitivity_identities):
        _fail("sensitivity_evidence contains duplicate profile/arm/scenario identities")
    if any(
        evidence["included_markets_sha256"]
        != scope["selected_market_set_sha256"]
        for evidence in manifest["sensitivity_evidence"]
    ):
        _fail("sensitivity_evidence does not bind to scope.selected_markets")

    metric_ids: list[str] = []
    markets = _list(manifest["markets"], "markets")
    market_ids: list[str] = []
    for index, value_market in enumerate(markets):
        where = f"markets[{index}]"
        market = _object(value_market, where, MARKET_FIELDS)
        market_ids.append(_string(market["market"], f"{where}.market"))
        _string(market["token0"], f"{where}.token0", allow_empty=True)
        _string(market["token1"], f"{where}.token1", allow_empty=True)
        _bool(market["has_lending_activity"], f"{where}.has_lending_activity")
        for lane in ("raw_metrics", "usd_metrics"):
            for metric_index, metric in enumerate(_list(market[lane], f"{where}.{lane}")):
                metric_ids.append(
                    _validate_named_metric(metric, f"{where}.{lane}[{metric_index}]")
                )
    if len(set(market_ids)) != len(market_ids):
        _fail("markets contains duplicate market identities")

    for index, metric in enumerate(_list(manifest["aggregate_metrics"], "aggregate_metrics")):
        metric_ids.append(_validate_named_metric(metric, f"aggregate_metrics[{index}]"))
    if len(set(metric_ids)) != len(metric_ids):
        _fail("metric_id values must be globally unique")

    for index, interval in enumerate(_list(manifest["bootstrap_intervals"], "bootstrap_intervals")):
        _validate_bootstrap(interval, f"bootstrap_intervals[{index}]")

    identity_ids = [
        _validate_conservation(identity, f"conservation_identities[{index}]")
        for index, identity in enumerate(
            _list(manifest["conservation_identities"], "conservation_identities")
        )
    ]
    if len(set(identity_ids)) != len(identity_ids):
        _fail("conservation identity IDs must be unique")
    _validate_accounting(manifest["accounting_verification"])
    _string_array(manifest["limitations"], "limitations")
    report_ids = _string_array(
        manifest["report_metric_ids"], "report_metric_ids", unique=True
    )
    unknown_report_ids = sorted(set(report_ids) - set(metric_ids))
    if unknown_report_ids:
        _fail(f"report_metric_ids references unknown metrics: {unknown_report_ids}")

    expected_hash = compute_manifest_sha256(manifest)
    if manifest["manifest_sha256"] != expected_hash:
        _fail(
            "manifest_sha256 does not match compact ResultManifestV1 serialization "
            f"(expected {expected_hash})"
        )
    return manifest


def load_and_validate_manifest(path: Path) -> dict[str, Any]:
    return validate_manifest(load_json_strict(path))


def _cell_source(cell: Mapping[str, Any], where: str) -> str:
    source = cell.get("source")
    if type(source) is str:
        return source
    if type(source) is list and all(type(line) is str for line in source):
        return "".join(source)
    raise NotebookValidationError(f"{where}.source must be a string or string array")


DENIED_CALLS = frozenset(
    {
        "__import__",
        "breakpoint",
        "compile",
        "delattr",
        "dir",
        "eval",
        "exec",
        "getattr",
        "globals",
        "help",
        "input",
        "locals",
        "open",
        "setattr",
        "vars",
    }
)


class _RestrictedAst(ast.NodeVisitor):
    def __init__(self, cell_index: int) -> None:
        self.cell_index = cell_index

    def _reject(self, node: ast.AST, reason: str) -> NoReturn:
        line = getattr(node, "lineno", "?")
        raise NotebookValidationError(
            f"code cell {self.cell_index}, line {line}: {reason}"
        )

    def visit_Import(self, node: ast.Import) -> NoReturn:  # noqa: N802
        self._reject(node, "imports are forbidden; metrics_manifest is the only data input")

    def visit_ImportFrom(self, node: ast.ImportFrom) -> NoReturn:  # noqa: N802
        self._reject(node, "imports are forbidden; metrics_manifest is the only data input")

    def visit_Global(self, node: ast.Global) -> NoReturn:  # noqa: N802
        self._reject(node, "global declarations are forbidden")

    def visit_Nonlocal(self, node: ast.Nonlocal) -> NoReturn:  # noqa: N802
        self._reject(node, "nonlocal declarations are forbidden")

    def visit_Name(self, node: ast.Name) -> None:  # noqa: N802
        if node.id.startswith("_"):
            self._reject(node, "private/dunder names are forbidden")

    def visit_Attribute(self, node: ast.Attribute) -> None:  # noqa: N802
        if node.attr.startswith("_"):
            self._reject(node, "private/dunder attributes are forbidden")
        self.generic_visit(node)

    def visit_Call(self, node: ast.Call) -> None:  # noqa: N802
        if isinstance(node.func, ast.Name) and node.func.id in DENIED_CALLS:
            self._reject(node, f"call to {node.func.id!r} is forbidden")
        if isinstance(node.func, ast.Attribute) and node.func.attr in DENIED_CALLS:
            self._reject(node, f"call to {node.func.attr!r} is forbidden")
        self.generic_visit(node)


def validate_notebook(value: Any) -> dict[str, Any]:
    if type(value) is not dict:
        raise NotebookValidationError("notebook root must be an object")
    if value.get("nbformat") != 4 or type(value.get("nbformat_minor")) is not int:
        raise NotebookValidationError("notebook must use nbformat 4")
    cells = value.get("cells")
    if type(cells) is not list or not cells:
        raise NotebookValidationError("notebook must contain cells")

    encountered_sections: list[str] = []
    code_index = 0
    for index, cell in enumerate(cells):
        where = f"cells[{index}]"
        if type(cell) is not dict:
            raise NotebookValidationError(f"{where} must be an object")
        cell_type = cell.get("cell_type")
        if cell_type not in {"markdown", "code"}:
            raise NotebookValidationError(f"{where} has unsupported cell_type {cell_type!r}")
        source = _cell_source(cell, where)
        if cell_type == "markdown":
            encountered_sections.extend(
                line.strip() for line in source.splitlines() if line.strip() in REQUIRED_SECTIONS
            )
            continue
        code_index += 1
        if len(source) > MAX_CODE_CHARACTERS:
            raise NotebookValidationError(f"code cell {code_index} exceeds the source bound")
        try:
            tree = ast.parse(source, filename=f"benchmark.ipynb:cell-{code_index}", mode="exec")
        except SyntaxError as exc:
            raise NotebookValidationError(f"code cell {code_index} has invalid Python: {exc}") from exc
        _RestrictedAst(code_index).visit(tree)

    if tuple(encountered_sections) != REQUIRED_SECTIONS:
        raise NotebookValidationError(
            "analysis sections must appear exactly once and in this order: "
            + " -> ".join(REQUIRED_SECTIONS)
        )
    return value


def load_and_validate_notebook(path: Path) -> dict[str, Any]:
    try:
        value = load_json_strict(path)
    except ManifestValidationError as exc:
        raise NotebookValidationError(str(exc)) from exc
    return validate_notebook(value)


def _freeze(value: Any) -> Any:
    if type(value) is dict:
        return MappingProxyType({key: _freeze(item) for key, item in value.items()})
    if type(value) is list:
        return tuple(_freeze(item) for item in value)
    return value


SAFE_BUILTINS = MappingProxyType(
    {
        "abs": abs,
        "all": all,
        "any": any,
        "bool": bool,
        "dict": dict,
        "enumerate": enumerate,
        "int": int,
        "len": len,
        "list": list,
        "max": max,
        "min": min,
        "print": print,
        "range": range,
        "reversed": reversed,
        "set": set,
        "sorted": sorted,
        "str": str,
        "sum": sum,
        "tuple": tuple,
        "zip": zip,
    }
)


class _BoundedOutput(io.StringIO):
    def write(self, text: str) -> int:
        projected_characters = self.tell() + len(text)
        projected_lines = self.getvalue().count("\n") + text.count("\n")
        if projected_characters > MAX_OUTPUT_CHARACTERS or projected_lines > MAX_OUTPUT_LINES:
            raise NotebookExecutionError(
                "notebook cell output exceeds the bounded-preview contract"
            )
        return super().write(text)


def _write_json_atomic(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary_name: str | None = None
    try:
        with tempfile.NamedTemporaryFile(
            "w",
            encoding="utf-8",
            dir=path.parent,
            prefix=f".{path.name}.",
            suffix=".tmp",
            delete=False,
        ) as temporary:
            temporary_name = temporary.name
            json.dump(value, temporary, ensure_ascii=False, allow_nan=False, indent=1)
            temporary.write("\n")
            temporary.flush()
            os.fsync(temporary.fileno())
        os.replace(temporary_name, path)
    finally:
        if temporary_name is not None and os.path.exists(temporary_name):
            os.unlink(temporary_name)


def execute_notebook(manifest_path: Path, notebook_path: Path, output_path: Path) -> int:
    # This order is a security property: no notebook content is parsed or run
    # until the manifest's schema, exact values, identities, and checksum pass.
    manifest = load_and_validate_manifest(manifest_path)
    notebook = load_and_validate_notebook(notebook_path)
    if notebook_path.resolve() == output_path.resolve():
        raise NotebookValidationError("output must be an executed copy, not the source notebook")

    executed = copy.deepcopy(notebook)
    namespace: dict[str, Any] = {
        "__builtins__": SAFE_BUILTINS,
        "metrics_manifest": _freeze(manifest),
    }
    execution_count = 0
    for cell in executed["cells"]:
        if cell["cell_type"] != "code":
            continue
        execution_count += 1
        source = _cell_source(cell, f"code cell {execution_count}")
        output = _BoundedOutput()
        try:
            with redirect_stdout(output):
                exec(
                    compile(
                        source,
                        filename=f"benchmark.ipynb:cell-{execution_count}",
                        mode="exec",
                    ),
                    namespace,
                    namespace,
                )
        except NotebookExecutionError:
            raise
        except Exception as exc:
            raise NotebookExecutionError(
                f"code cell {execution_count} failed: {type(exc).__name__}: {exc}"
            ) from exc
        cell["execution_count"] = execution_count
        text = output.getvalue()
        cell["outputs"] = [] if not text else [
            {
                "output_type": "stream",
                "name": "stdout",
                "text": text.splitlines(keepends=True),
            }
        ]

    executed.setdefault("metadata", {})["benchmark_execution"] = {
        "metrics_manifest_sha256": manifest["manifest_sha256"],
        "restricted_stdlib_executor": True,
        "code_cells_executed": execution_count,
    }
    _write_json_atomic(output_path, executed)
    return execution_count


def _coverage_for_test() -> dict[str, Any]:
    metric = {
        "availability": "available",
        "covered_numerator": "1",
        "total_denominator": "1",
        "required_bps": 9_000,
        "actual_bps": 10_000,
        "passes": True,
    }
    return {
        "visible_swap_volume": dict(metric),
        "borrowed_notional": dict(metric),
        "revenue_notional": dict(metric),
        "exact_transaction_fetch_bps": 10_000,
        "exact_decode_bps": 10_000,
        "v1_zero_difference_reconciled": True,
    }


def _metric_value_for_test(numerator: str, denominator: str) -> dict[str, Any]:
    return {
        "availability": "available",
        "numerator": numerator,
        "denominator": denominator,
        "unit": "usd_micro",
        "denominator_definition": None,
    }


def _valid_manifest_for_test() -> dict[str, Any]:
    digest = "1" * 64
    metric_id = "aggregate.profile.profile-a.arm.intent.scenario.no_hlp.test"
    selected_markets = ["market-a", "market-b"]
    cohort_sha256 = "b" * 64
    final_gate_sha256 = "d" * 64
    manifest: dict[str, Any] = {
        "schema_version": 3,
        "run_id": "self-test-run",
        "dataset_id": "0" * 64,
        "profile_freeze_sha256": digest,
        "dusk_source_sha256": "2" * 64,
        "cutoff_slot": 10,
        "scope": {
            "scope_mode": "hybrid_panel",
            "evidence_tier": "decision_grade",
            "selected_markets": selected_markets,
            "selected_market_set_sha256": _market_set_sha256(selected_markets),
            "cohort_selection_sha256": cohort_sha256,
            "cohort_representativeness": {
                "artifact_sha256": "c" * 64,
                "cohort_selection_sha256": cohort_sha256,
                "swaps": {
                    "visible_universe_rows": 100,
                    "selected_panel_rows": 80,
                    "selected_panel_coverage_bps": 8_000,
                },
                "debt_actions": {
                    "visible_universe_rows": 40,
                    "selected_panel_rows": 20,
                    "selected_panel_coverage_bps": 5_000,
                },
                "liquidations": {
                    "visible_universe_rows": 4,
                    "selected_panel_rows": 3,
                    "selected_panel_coverage_bps": 7_500,
                },
            },
            "final_phase_gate_sha256": final_gate_sha256,
            "final_phase_gate_decision": {
                "phase": "post_replay_final",
                "evidence_tier": "decision_grade",
                "phase_eligible": True,
                "raw_market_eligible": True,
                "usd_headline_eligible": True,
                "raw_certification": "limited",
                "usd_certification": "limited",
                "raw_blocking_gates": [],
                "usd_blocking_gates": [],
                "limitations": [
                    {
                        "gate": "decoder_revision_certification",
                        "underlying_status": "fail",
                        "detail_code": "decoder_revision_certification_pending",
                    },
                    {
                        "gate": "terminal_state",
                        "underlying_status": "fail",
                        "detail_code": "terminal_state_snapshot_unavailable",
                    },
                ],
            },
            "legacy_global_profile_semantics": LEGACY_GLOBAL_PROFILE_SEMANTICS,
        },
        "common_window_start_unix_seconds": 100,
        "common_window_end_unix_seconds_exclusive": 1_000,
        "holdout_start_unix_seconds": 800,
        "holdout_end_unix_seconds_exclusive": 1_000,
        "holdout_evidence_sha256": "3" * 64,
        "universe_window_sha256": "4" * 64,
        "source_artifact_sha256": {
            "paired-ledger.jsonl": "5" * 64,
            "results/cohort-selection.json": "9" * 64,
            "results/cohort-representativeness.json": "a" * 64,
            "results/v1-final-phase-gate.json": final_gate_sha256,
        },
        "coverage": _coverage_for_test(),
        "headline_hlp_scenario": "maximum_feasible",
        "no_hlp_control_included": True,
        "global_winner_profile_id": "profile-a",
        "no_global_winner": False,
        "pareto_profile_ids": ["profile-a"],
        "per_market_calibration_qualification_sha256": "6" * 64,
        "per_market_holdout_qualification_sha256": "7" * 64,
        "per_market_opportunity_ceilings": [],
        "sensitivity_evidence": [],
        "markets": [],
        "aggregate_metrics": [
            {
                "metric_id": metric_id,
                "family": "lp_revenue",
                "profile_id": "profile-a",
                "replay_arm": "intent",
                "hlp_scenario": "no_hlp",
                "definition": "Self-test exact value.",
                "revenue_dimensions": None,
                "v1": _metric_value_for_test("2", "1"),
                "dusk": _metric_value_for_test("13", "3"),
                "paired_delta": _metric_value_for_test("7", "3"),
            }
        ],
        "bootstrap_intervals": [],
        "conservation_identities": [],
        "accounting_verification": {
            "v1_conservation_checks": 1,
            "v1_conservation_failures": 0,
            "dusk_atomic_transition_checks": 1,
            "dusk_atomic_transition_failures": 0,
            "paired_revenue_lane_checks": 1,
            "paired_revenue_lane_failures": 0,
        },
        "limitations": ["Self-test fixture; not benchmark evidence."],
        "report_metric_ids": [metric_id],
        "manifest_sha256": "0" * 64,
    }
    manifest["manifest_sha256"] = compute_manifest_sha256(manifest)
    return manifest


def _seal_for_test(manifest: dict[str, Any]) -> None:
    manifest["manifest_sha256"] = compute_manifest_sha256(manifest)


def _expect_failure(exception: type[BaseException], operation: Any) -> None:
    try:
        operation()
    except exception:
        return
    raise AssertionError(f"expected {exception.__name__}")


def run_self_test() -> None:
    with tempfile.TemporaryDirectory(prefix="v1-vs-dusk-notebook-test-") as directory:
        root = Path(directory)
        manifest_path = root / "metrics-manifest.json"
        output_path = root / "benchmark.executed.ipynb"
        manifest = _valid_manifest_for_test()
        _write_json_atomic(manifest_path, manifest)

        cells = execute_notebook(manifest_path, DEFAULT_NOTEBOOK, output_path)
        if cells < 1 or "7/3 usd_micro" not in output_path.read_text(encoding="utf-8"):
            raise AssertionError("executed notebook did not preserve the exact rational string")
        first_execution = output_path.read_bytes()
        execute_notebook(manifest_path, DEFAULT_NOTEBOOK, output_path)
        if first_execution != output_path.read_bytes():
            raise AssertionError("executed notebook output is not deterministic")

        _expect_failure(
            ManifestValidationError,
            lambda: loads_json_strict('{"x":1,"x":2}', source="duplicate-test"),
        )

        bad_hash = copy.deepcopy(manifest)
        bad_hash["manifest_sha256"] = "f" * 64
        _expect_failure(ManifestValidationError, lambda: validate_manifest(bad_hash))

        bad_identity = copy.deepcopy(manifest)
        bad_identity["source_artifact_sha256"] = {}
        _seal_for_test(bad_identity)
        _expect_failure(ManifestValidationError, lambda: validate_manifest(bad_identity))

        bad_winner = copy.deepcopy(manifest)
        bad_winner["no_global_winner"] = True
        _seal_for_test(bad_winner)
        _expect_failure(ManifestValidationError, lambda: validate_manifest(bad_winner))

        missing_scope = copy.deepcopy(manifest)
        del missing_scope["scope"]
        _expect_failure(
            ManifestValidationError, lambda: validate_manifest(missing_scope)
        )

        bad_scope_hash = copy.deepcopy(manifest)
        bad_scope_hash["scope"]["selected_market_set_sha256"] = "e" * 64
        _seal_for_test(bad_scope_hash)
        _expect_failure(
            ManifestValidationError, lambda: validate_manifest(bad_scope_hash)
        )

        bad_representativeness = copy.deepcopy(manifest)
        bad_representativeness["scope"]["cohort_representativeness"]["swaps"][
            "selected_panel_rows"
        ] = 101
        _seal_for_test(bad_representativeness)
        _expect_failure(
            ManifestValidationError,
            lambda: validate_manifest(bad_representativeness),
        )

        bad_gate_limitation = copy.deepcopy(manifest)
        bad_gate_limitation["scope"]["final_phase_gate_decision"]["limitations"][
            0
        ]["gate"] = "economic_replay"
        _seal_for_test(bad_gate_limitation)
        _expect_failure(
            ManifestValidationError, lambda: validate_manifest(bad_gate_limitation)
        )

        bad_exact = copy.deepcopy(manifest)
        bad_exact["aggregate_metrics"][0]["paired_delta"]["availability"] = "not_evaluated"
        _seal_for_test(bad_exact)
        _write_json_atomic(manifest_path, bad_exact)
        blocked_output = root / "must-not-exist.ipynb"
        _expect_failure(
            ManifestValidationError,
            lambda: execute_notebook(manifest_path, DEFAULT_NOTEBOOK, blocked_output),
        )
        if blocked_output.exists():
            raise AssertionError("invalid manifest reached notebook execution/output")

        bad_notebook = copy.deepcopy(load_and_validate_notebook(DEFAULT_NOTEBOOK))
        bad_notebook["cells"].append(
            {
                "cell_type": "code",
                "execution_count": None,
                "metadata": {},
                "outputs": [],
                "source": ["import os\n"],
            }
        )
        _expect_failure(NotebookValidationError, lambda: validate_notebook(bad_notebook))

    print(
        "self-test passed: checksum, identity, scope/cohort/final-gate, duplicate-key, "
        "winner, exact-value, sandbox, and deterministic execution checks"
    )


def _parse_args(argv: Iterable[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, help="sealed metrics-manifest.json")
    parser.add_argument("--notebook", type=Path, default=DEFAULT_NOTEBOOK)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--self-test", action="store_true")
    return parser.parse_args(argv)


def main(argv: Iterable[str] | None = None) -> int:
    args = _parse_args(argv)
    if args.self_test:
        run_self_test()
        return 0
    if args.manifest is None:
        raise SystemExit("--manifest is required (or run --self-test)")
    output = args.output or args.notebook.with_name(f"{args.notebook.stem}.executed.ipynb")
    count = execute_notebook(args.manifest, args.notebook, output)
    manifest = load_and_validate_manifest(args.manifest)
    print(
        f"executed {count} code cells -> {output} "
        f"(manifest {manifest['manifest_sha256']})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
