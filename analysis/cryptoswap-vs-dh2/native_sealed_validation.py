#!/usr/bin/env python3
"""Run post-lock historical validation without reopening profile selection.

The profile lock is materialized from the canonical synthetic selection before
sealed history is fetched.  Replay mode reconstructs exactly those three
locked profiles, verifies every input hash, evaluates the full historical
cross, and writes diagnostic outputs to ``results/sealed_validation`` only.
Nothing in this module ranks profiles or feeds the canonical selector.
"""

from __future__ import annotations

import argparse
import csv
from dataclasses import asdict, fields
from datetime import datetime, timezone
import json
import math
from pathlib import Path
import statistics
import tempfile
from typing import Any, Mapping, Sequence

from curve_native import CurveNativeConfig
from dusk_policy import DynamicFeeConfig
from economics import ControlledConfig
from fetch_native_history import (
    BURN_IN_MINUTES,
    SCORE_MINUTES,
    SLICE_MINUTES,
    START_STRIDE_MINUTES,
)
from models import CryptoSwap, CurrentDh2
from native_benchmark import (
    FLOW_REGIMES,
    POOL_VALUE,
    PRIMARY_SEEDS,
    PRIMARY_STEP_MINUTES,
    NativeProfile,
    _cpmm_profiles,
    _curve_profiles,
    _dusk_profiles,
    _job,
    _matched_models,
    _run_jobs,
)
from scenarios import NativeHistoricalWindow
from sealed_protocol import (
    DATA,
    LOCK_PATH,
    LOCK_SCHEMA_VERSION,
    LOCKED_SYSTEMS,
    ROOT,
    SEALED_CSV_PATH,
    SEALED_ENGINE_FILES,
    SEALED_HISTORY_SCHEMA_VERSION,
    SEALED_MANIFEST_PATH,
    SEALED_METADATA_PATH,
    SEALED_MONTHS,
    SEALED_OUTPUT_DIR,
    SEALED_OUTPUT_SCHEMA_VERSION,
    SEALED_ROUTES,
    SEALED_STRATA,
    canonical_fingerprint,
    engine_hashes,
    profile_lock_fingerprint,
    search_source_bindings,
    sha256_path,
    verify_postlock_hashes,
    verify_profile_lock,
)


PROMOTION_SOURCE_PATHS = {
    "curve_native_v2_1": "data/curve_controller_profiles.json",
    "dusk_native": "data/dusk_controller_profiles.json",
    "dusk_cpmm": "data/cpmm_controller_profiles.json",
}


EXPECTED_WINDOWS = len(SEALED_MONTHS) * len(SEALED_STRATA)
EXPECTED_BASE_CELLS = (
    EXPECTED_WINDOWS * len(FLOW_REGIMES) * len(PRIMARY_SEEDS) * len(SEALED_ROUTES)
)
EXPECTED_SEALED_ROWS = EXPECTED_BASE_CELLS * len(LOCKED_SYSTEMS)
PAIR_SPECS = (
    ("dusk_native", "curve_native_v2_1", "dusk_minus_curve"),
    ("dusk_native", "dusk_cpmm", "dusk_minus_cpmm"),
    ("curve_native_v2_1", "dusk_cpmm", "curve_minus_cpmm"),
)
EXPECTED_PAIRED_ROWS = EXPECTED_BASE_CELLS * len(PAIR_SPECS)

DECISION_FIELDS = (
    "net_lp_return_after_protocol_fee_bps",
    "net_lp_return_vs_hold_bps",
    "selected_execution_cost_bps",
    "captured_user_volume_fraction",
    "depth_degradation_fraction",
    "marginal_tracking_stale_fraction",
    "accounting_identity_error",
)


def _utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _parse_utc(value: object) -> datetime:
    text = str(value)
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    parsed = datetime.fromisoformat(text)
    if parsed.tzinfo is None:
        raise ValueError("sealed timestamp must include a timezone")
    return parsed.astimezone(timezone.utc)


def _exact_dataclass_payload(
    payload: Mapping[str, object],
    dataclass_type: type,
    *,
    label: str,
) -> dict[str, object]:
    expected = {field.name for field in fields(dataclass_type)}
    if set(payload) != expected:
        missing = sorted(expected - set(payload))
        extra = sorted(set(payload) - expected)
        raise ValueError(f"{label} fields mismatch: missing={missing}, extra={extra}")
    return dict(payload)


def _profile_inventory() -> dict[tuple[str, str], NativeProfile]:
    curve, dusk = _matched_models()
    curve_profiles = _curve_profiles(curve)
    dusk_profiles = _dusk_profiles(dusk)
    profiles = (*curve_profiles, *dusk_profiles, *_cpmm_profiles(dusk_profiles))
    inventory: dict[tuple[str, str], NativeProfile] = {}
    for profile in profiles:
        key = (profile.system, profile.profile_id)
        if key in inventory:
            raise AssertionError(f"duplicate lock candidate {key}")
        inventory[key] = profile
    return inventory


def _serialize_profile(profile: NativeProfile) -> dict[str, object]:
    return {
        "system": profile.system,
        "profile_id": profile.profile_id,
        "description": profile.description,
        "model_type": type(profile.model).__name__,
        "model": asdict(profile.model),
        "config_type": type(profile.config).__name__,
        "config": asdict(profile.config),
    }


def _payload_fingerprint(payload: Mapping[str, object]) -> str:
    return canonical_fingerprint({"payload": dict(payload)})


def _load_promoted_profile(
    system: str,
    profile_id: str,
    *,
    expected_sha256: object,
    expected_fingerprint: object,
) -> tuple[dict[str, object], dict[str, object]]:
    """Load one exact promoted row and verify its immutable source binding."""

    relative = PROMOTION_SOURCE_PATHS[system]
    path = ROOT / relative
    if not path.is_file():
        raise FileNotFoundError(f"missing canonical promotion snapshot: {path}")
    digest = sha256_path(path)
    if digest != str(expected_sha256):
        raise ValueError(f"canonical {system} promotion source hash mismatch")
    payload = json.loads(path.read_text())
    if not isinstance(payload, dict) or payload.get("schema_version") != 2:
        raise ValueError(f"unsupported canonical {system} promotion schema")
    if payload.get("system") != system:
        raise ValueError(f"canonical promotion system mismatch for {system}")
    unsigned = dict(payload)
    fingerprint = str(unsigned.pop("promotion_fingerprint", ""))
    if (
        fingerprint != str(expected_fingerprint)
        or fingerprint != canonical_fingerprint(unsigned)
    ):
        raise ValueError(f"canonical {system} promotion fingerprint mismatch")
    if str(payload.get("winner_profile_id", "")) != profile_id:
        raise ValueError(f"canonical {system} selection is not the search winner")
    rows = payload.get("profiles")
    if not isinstance(rows, list):
        raise ValueError(f"canonical {system} promotion profile list is missing")
    matches = [
        row
        for row in rows
        if isinstance(row, dict) and str(row.get("profile_id", "")) == profile_id
    ]
    if len(matches) != 1:
        raise ValueError(f"canonical {system} winner is not unique in its promotion")
    row = dict(matches[0])
    if row.get("selected_winner") is not True or int(row.get("source_rank", -1)) != 1:
        raise ValueError(f"canonical {system} winner is not promotion rank one")
    if not isinstance(row.get("model"), dict) or not isinstance(
        row.get("config"), dict
    ):
        raise ValueError(f"canonical {system} promotion lacks exact model/config")
    return row, {
        "path": relative,
        "sha256": digest,
        "promotion_fingerprint": fingerprint,
    }


def _search_fingerprint_for_system(
    system: str,
    search_source: Mapping[str, object],
) -> object:
    fingerprint = search_source.get("search_fingerprint")
    if system == "dusk_native":
        if not isinstance(fingerprint, dict):
            raise ValueError("Dusk search fingerprint binding is missing")
        return fingerprint.get("grid")
    return fingerprint


def build_profile_lock_payload(
    *,
    summary_path: Path = ROOT / "results" / "native_summary.json",
    locked_at_utc: str | None = None,
) -> dict[str, object]:
    """Build a deterministic, self-contained lock from canonical selection."""

    if not summary_path.exists():
        raise FileNotFoundError(f"missing canonical native summary: {summary_path}")
    summary = json.loads(summary_path.read_text())
    validation = summary.get("validation", {})
    if summary.get("status") != "complete" or validation.get(
        "canonical_output"
    ) is not True:
        raise ValueError("profile lock requires a complete canonical native run")
    post_run_audit = validation.get("post_run_audit")
    promotion_audit = validation.get("search_promotion_audit")
    if not isinstance(post_run_audit, dict) or post_run_audit.get("status") != "passed":
        raise ValueError("profile lock requires the passed canonical post-run audit")
    if (
        not isinstance(promotion_audit, dict)
        or promotion_audit.get("status") != "passed"
        or promotion_audit.get("symmetric_concentrated_search") is not True
        or promotion_audit.get("independent_cpmm_search") is not True
        or set(promotion_audit.get("promotions", {})) != set(LOCKED_SYSTEMS)
    ):
        raise ValueError("profile lock requires all three passed search promotions")
    selection = summary.get("profile_selection")
    if not isinstance(selection, dict) or set(selection) != set(LOCKED_SYSTEMS):
        raise ValueError("canonical summary does not select exactly three systems")

    search_sources = search_source_bindings()
    search_name_by_system = {
        "curve_native_v2_1": "curve_controller_search",
        "dusk_native": "dusk_controller_search",
        "dusk_cpmm": "cpmm_controller_search",
    }
    inventory = _profile_inventory()
    locked: dict[str, dict[str, object]] = {}
    binding: dict[str, dict[str, object]] = {}
    for system in LOCKED_SYSTEMS:
        selected = selection[system]
        if not isinstance(selected, dict):
            raise TypeError(f"invalid canonical selection for {system}")
        profile_id = str(selected.get("profile_id", ""))
        profile = inventory.get((system, profile_id))
        if profile is None:
            raise ValueError(f"selected profile is absent from inventory: {system}:{profile_id}")
        if str(selected.get("description", "")) != profile.description:
            raise ValueError(f"selected profile description mismatch: {system}:{profile_id}")
        audited = promotion_audit["promotions"][system]
        if (
            audited.get("canonical_locked_profile_id") != profile_id
            or audited.get("search_winner_profile_id") != profile_id
        ):
            raise ValueError(
                f"canonical {system} profile is not the audited full-search winner"
            )
        search_source = search_sources[search_name_by_system[system]]
        if (
            audited.get("search_summary_sha256") != search_source.get("sha256")
            or audited.get("search_fingerprint")
            != _search_fingerprint_for_system(system, search_source)
            or profile_id
            != str(search_source.get("winner", {}).get("profile_id", ""))
        ):
            raise ValueError(f"canonical {system} search audit is stale")
        promoted, promotion_source = _load_promoted_profile(
            system,
            profile_id,
            expected_sha256=audited.get("promotion_sha256"),
            expected_fingerprint=audited.get("promotion_fingerprint"),
        )
        serialized = _serialize_profile(profile)
        if (
            str(promoted.get("description", "")) != serialized["description"]
            or _payload_fingerprint(promoted["model"])
            != _payload_fingerprint(serialized["model"])
            or _payload_fingerprint(promoted["config"])
            != _payload_fingerprint(serialized["config"])
        ):
            raise ValueError(
                f"canonical {system} promoted model/config differs from executable inventory"
            )
        locked[system] = serialized
        binding[system] = {
            "profile_id": profile.profile_id,
            "description": profile.description,
            "model_fingerprint": _payload_fingerprint(serialized["model"]),
            "config_fingerprint": _payload_fingerprint(serialized["config"]),
            "promotion_source": promotion_source,
        }

    if not isinstance(summary_path, Path):
        summary_path = Path(summary_path)
    try:
        relative_summary = summary_path.resolve().relative_to(ROOT.resolve())
    except ValueError as error:
        raise ValueError("canonical summary must live under the benchmark root") from error
    timestamp = locked_at_utc or _utc_now()
    _parse_utc(timestamp)
    payload: dict[str, object] = {
        "schema_version": LOCK_SCHEMA_VERSION,
        "purpose": "pre-history immutable lock for diagnostic sealed validation",
        "locked_at_utc": timestamp,
        "selection_source": {
            "path": str(relative_summary),
            "sha256": sha256_path(summary_path),
            "status": "complete",
            "canonical_output": True,
        },
        "search_sources": search_sources,
        "selection_binding": binding,
        "locked_profiles": locked,
        "dataset_protocol": {
            "source_months": list(SEALED_MONTHS),
            "month_rule": (
                "five months beginning 2025-05 at fixed three-calendar-month intervals"
            ),
            "strata": list(SEALED_STRATA),
            "stratum_rule": (
                "existing deterministic lowest, median-nearest, and highest "
                "12-hour close-price range among 12-hour-grid candidates; "
                "non-overlapping 24-hour burn-in plus 12-hour score slices"
            ),
            "burn_in_minutes": BURN_IN_MINUTES,
            "score_minutes": SCORE_MINUTES,
            "candidate_stride_minutes": START_STRIDE_MINUTES,
            "expected_windows": EXPECTED_WINDOWS,
        },
        "matrix_protocol": {
            "systems": list(LOCKED_SYSTEMS),
            "flow_regimes": [dict(flow) for flow in FLOW_REGIMES],
            "seeds": list(PRIMARY_SEEDS),
            "routing_modes": list(SEALED_ROUTES),
            "step_minutes": PRIMARY_STEP_MINUTES,
            "pool_value": POOL_VALUE,
            "expected_base_cells": EXPECTED_BASE_CELLS,
            "expected_result_rows": EXPECTED_SEALED_ROWS,
            "selection_performed": False,
            "feeds_profile_selection": False,
        },
        "engine_hashes": engine_hashes(),
        "pre_history_assertion": (
            "this lock contains no sealed-history prices, metrics, outcomes, or winner"
        ),
    }
    payload["profile_lock_fingerprint"] = profile_lock_fingerprint(payload)
    return payload


def materialize_profile_lock(
    *,
    path: Path = LOCK_PATH,
    summary_path: Path = ROOT / "results" / "native_summary.json",
    locked_at_utc: str | None = None,
) -> dict[str, object]:
    """Atomically create the one-way profile lock before history exists."""

    forbidden = (
        path,
        SEALED_CSV_PATH,
        SEALED_METADATA_PATH,
        SEALED_MANIFEST_PATH,
        SEALED_OUTPUT_DIR / "sealed_native_results.csv",
        SEALED_OUTPUT_DIR / "sealed_native_paired.csv",
        SEALED_OUTPUT_DIR / "sealed_native_summary.json",
    )
    existing = [str(candidate) for candidate in forbidden if candidate.exists()]
    if existing:
        raise FileExistsError(
            "profile locking is one-way; sealed artifacts already exist: "
            + ", ".join(existing)
        )
    payload = build_profile_lock_payload(
        summary_path=summary_path,
        locked_at_utc=locked_at_utc,
    )
    reconstruct_locked_profiles(payload)
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".profile-lock-stage-", dir=path.parent) as temp:
        staged = Path(temp) / path.name
        staged.write_text(
            json.dumps(payload, indent=2, allow_nan=False) + "\n"
        )
        staged.replace(path)
    verified = verify_profile_lock(path, verify_environment=True)
    reconstruct_locked_profiles(verified)
    return verified


def _verify_locked_profile_binding(
    lock: Mapping[str, object],
    system: str,
    row: Mapping[str, object],
) -> None:
    bindings = lock.get("selection_binding")
    if not isinstance(bindings, dict) or set(bindings) != set(LOCKED_SYSTEMS):
        raise ValueError("sealed lock selection binding must cover all three systems")
    binding = bindings[system]
    if not isinstance(binding, dict):
        raise ValueError(f"sealed lock selection binding is invalid for {system}")
    if (
        str(binding.get("profile_id", "")) != str(row.get("profile_id", ""))
        or str(binding.get("description", "")) != str(row.get("description", ""))
    ):
        raise ValueError(f"sealed {system} selection identity binding mismatch")
    model = row.get("model")
    config = row.get("config")
    if not isinstance(model, dict) or not isinstance(config, dict):
        raise ValueError(f"sealed {system} locked model/config is missing")
    if (
        binding.get("model_fingerprint") != _payload_fingerprint(model)
        or binding.get("config_fingerprint") != _payload_fingerprint(config)
    ):
        raise ValueError(f"sealed {system} locked model/config binding mismatch")
    source = binding.get("promotion_source")
    if not isinstance(source, dict) or source.get("path") != PROMOTION_SOURCE_PATHS[
        system
    ]:
        raise ValueError(f"sealed {system} promotion source binding is missing")
    promoted, actual_source = _load_promoted_profile(
        system,
        str(row["profile_id"]),
        expected_sha256=source.get("sha256"),
        expected_fingerprint=source.get("promotion_fingerprint"),
    )
    if actual_source != source:
        raise ValueError(f"sealed {system} promotion source binding mismatch")
    if (
        str(promoted.get("description", "")) != str(row.get("description", ""))
        or _payload_fingerprint(promoted["model"])
        != _payload_fingerprint(model)
        or _payload_fingerprint(promoted["config"])
        != _payload_fingerprint(config)
    ):
        raise ValueError(f"sealed {system} locked profile differs from its promotion")


def reconstruct_locked_profiles(lock: Mapping[str, object]) -> dict[str, NativeProfile]:
    """Reconstruct only the three fully materialized locked profiles."""

    rows = lock.get("locked_profiles")
    if not isinstance(rows, dict) or set(rows) != set(LOCKED_SYSTEMS):
        raise ValueError("invalid locked profile set")
    output: dict[str, NativeProfile] = {}
    for system in LOCKED_SYSTEMS:
        row = rows[system]
        if not isinstance(row, dict):
            raise TypeError(f"locked profile {system} must be an object")
        _verify_locked_profile_binding(lock, system, row)
        model_payload = row.get("model")
        config_payload = row.get("config")
        if not isinstance(model_payload, dict) or not isinstance(config_payload, dict):
            raise TypeError(f"locked profile {system} has invalid model/config")
        if system == "curve_native_v2_1":
            if row.get("model_type") != "CryptoSwap" or row.get(
                "config_type"
            ) != "CurveNativeConfig":
                raise ValueError("locked Curve types mismatch")
            model = CryptoSwap(
                **_exact_dataclass_payload(
                    model_payload, CryptoSwap, label="locked Curve model"
                )
            )
            config = CurveNativeConfig(
                **_exact_dataclass_payload(
                    config_payload,
                    CurveNativeConfig,
                    label="locked Curve config",
                )
            )
            config.validate()
        else:
            if row.get("model_type") != "CurrentDh2" or row.get(
                "config_type"
            ) != "ControlledConfig":
                raise ValueError(f"locked Dusk types mismatch for {system}")
            model = CurrentDh2(
                **_exact_dataclass_payload(
                    model_payload, CurrentDh2, label=f"locked {system} model"
                )
            )
            config_values = _exact_dataclass_payload(
                config_payload,
                ControlledConfig,
                label=f"locked {system} config",
            )
            fee_payload = config_values["fee"]
            if not isinstance(fee_payload, dict):
                raise TypeError(f"locked {system} fee config must be an object")
            fee = DynamicFeeConfig(
                **_exact_dataclass_payload(
                    fee_payload,
                    DynamicFeeConfig,
                    label=f"locked {system} fee",
                )
            )
            fee.validate()
            config_values["fee"] = fee
            config = ControlledConfig(**config_values)
            if not (
                config.center_ema_half_life_minutes > 0
                and config.adjustment_threshold >= config.adjustment_step > 0
                and config.min_adjustment_interval_minutes > 0
                and 0 <= config.manager_fee_bps <= 10_000
                and 0 <= config.protocol_fee_bps <= 10_000
                and config.manager_fee_bps + config.protocol_fee_bps <= 10_000
            ):
                raise ValueError(f"locked {system} controller is invalid")
            if system == "dusk_cpmm" and not (
                model.peak_depth == 0.0 and model.fade_scale == 0.0
            ):
                raise ValueError("locked CPMM is not exact zero-concentration DH2")
            if system == "dusk_native" and not (
                model.peak_depth > 0.0 and model.fade_scale > 0.0
            ):
                raise ValueError("locked DH2 is not concentrated")
        output[system] = NativeProfile(
            system=system,
            profile_id=str(row["profile_id"]),
            model=model,
            config=config,
            description=str(row["description"]),
        )
    return output


def _verify_locked_matrix_protocol(lock: Mapping[str, object]) -> None:
    matrix = lock.get("matrix_protocol")
    if not isinstance(matrix, dict):
        raise ValueError("missing sealed matrix protocol")
    expected = {
        "systems": list(LOCKED_SYSTEMS),
        "flow_regimes": [dict(flow) for flow in FLOW_REGIMES],
        "seeds": list(PRIMARY_SEEDS),
        "routing_modes": list(SEALED_ROUTES),
        "step_minutes": PRIMARY_STEP_MINUTES,
        "pool_value": POOL_VALUE,
        "expected_base_cells": EXPECTED_BASE_CELLS,
        "expected_result_rows": EXPECTED_SEALED_ROWS,
        "selection_performed": False,
        "feeds_profile_selection": False,
    }
    if matrix != expected:
        raise ValueError("sealed replay matrix differs from the pre-history lock")


def load_and_verify_sealed_history(
    *,
    lock_path: Path = LOCK_PATH,
    csv_path: Path = SEALED_CSV_PATH,
    metadata_path: Path = SEALED_METADATA_PATH,
    manifest_path: Path = SEALED_MANIFEST_PATH,
) -> tuple[dict[str, object], dict[str, NativeHistoricalWindow], dict[str, str]]:
    lock = verify_profile_lock(lock_path, verify_environment=True)
    hashes = verify_postlock_hashes(
        lock_path=lock_path,
        csv_path=csv_path,
        metadata_path=metadata_path,
        manifest_path=manifest_path,
    )
    metadata = json.loads(metadata_path.read_text())
    manifest = json.loads(manifest_path.read_text())
    if metadata.get("schema_version") != SEALED_HISTORY_SCHEMA_VERSION:
        raise ValueError("unsupported sealed history metadata schema")
    if manifest.get("history_csv_path") != f"data/{SEALED_CSV_PATH.name}" or manifest.get(
        "history_metadata_path"
    ) != f"data/{SEALED_METADATA_PATH.name}":
        raise ValueError("sealed history manifest path mismatch")
    if metadata.get("sealed_post_lock") is not True:
        raise ValueError("history metadata is not marked post-lock")
    if metadata.get("sealed_profile_lock_fingerprint") != lock.get(
        "profile_lock_fingerprint"
    ):
        raise ValueError("history metadata profile-lock fingerprint mismatch")
    if _parse_utc(metadata.get("generated_at_utc")) < _parse_utc(
        lock.get("locked_at_utc")
    ):
        raise ValueError("sealed history predates its profile lock")
    dataset = lock["dataset_protocol"]
    if metadata.get("selection_protocol_fingerprint") != canonical_fingerprint(
        dataset
    ):
        raise ValueError("sealed history selection protocol mismatch")
    if tuple(metadata.get("source_months", ())) != SEALED_MONTHS:
        raise ValueError("sealed history month set mismatch")
    if int(metadata.get("burn_in_minutes", -1)) != BURN_IN_MINUTES or int(
        metadata.get("score_minutes", -1)
    ) != SCORE_MINUTES:
        raise ValueError("sealed history phase lengths mismatch")
    expected_history_rows = EXPECTED_WINDOWS * SLICE_MINUTES
    if int(metadata.get("rows", -1)) != expected_history_rows:
        raise ValueError("sealed history metadata row count mismatch")

    sources = metadata.get("sources")
    if not isinstance(sources, list) or len(sources) != len(SEALED_MONTHS):
        raise ValueError("sealed source archive manifest is incomplete")
    for month, source in zip(SEALED_MONTHS, sources):
        if not isinstance(source, dict) or not str(source.get("url", "")).endswith(
            f"SOLUSDT-1m-{month}.zip"
        ):
            raise ValueError(f"sealed source archive order mismatch for {month}")
        digest = str(source.get("sha256", ""))
        if len(digest) != 64 or any(character not in "0123456789abcdef" for character in digest):
            raise ValueError(f"invalid sealed source archive hash for {month}")
        if int(source.get("bytes", 0)) <= 0:
            raise ValueError(f"empty sealed source archive for {month}")

    required_fields = {
        "window_id",
        "stratum",
        "phase",
        "offset_minute",
        "timestamp_ms",
        "close",
        "quote_volume",
        "trades",
        "source_month",
    }
    grouped: dict[str, list[dict[str, str]]] = {}
    with csv_path.open() as source:
        reader = csv.DictReader(source)
        if reader.fieldnames is None or not required_fields.issubset(reader.fieldnames):
            raise ValueError("sealed history CSV columns are incomplete")
        for row in reader:
            grouped.setdefault(row["window_id"], []).append(row)
    expected_ids = {
        f"{month}_{stratum}"
        for month in SEALED_MONTHS
        for stratum in SEALED_STRATA
    }
    if set(grouped) != expected_ids:
        raise ValueError("sealed history window cross mismatch")
    if sum(len(rows) for rows in grouped.values()) != expected_history_rows:
        raise ValueError("sealed history CSV row count mismatch")

    metadata_windows = metadata.get("windows")
    if not isinstance(metadata_windows, list):
        raise ValueError("sealed window metadata is missing")
    by_metadata_id = {
        str(row.get("window_id")): row
        for row in metadata_windows
        if isinstance(row, dict)
    }
    if len(by_metadata_id) != EXPECTED_WINDOWS or set(by_metadata_id) != expected_ids:
        raise ValueError("sealed window metadata cross mismatch")

    windows: dict[str, NativeHistoricalWindow] = {}
    for window_id in sorted(grouped):
        rows = sorted(grouped[window_id], key=lambda row: int(row["offset_minute"]))
        if len(rows) != SLICE_MINUTES:
            raise ValueError(f"sealed window length mismatch: {window_id}")
        month, stratum = window_id.rsplit("_", 1)
        if month not in SEALED_MONTHS or stratum not in SEALED_STRATA:
            raise ValueError(f"invalid sealed window identity: {window_id}")
        timestamps: list[int] = []
        prices: list[float] = []
        for offset, row in enumerate(rows):
            expected_phase = "burn_in" if offset < BURN_IN_MINUTES else "score"
            if (
                int(row["offset_minute"]) != offset
                or row["phase"] != expected_phase
                or row["source_month"] != month
                or row["stratum"] != stratum
            ):
                raise ValueError(f"sealed row protocol mismatch: {window_id}:{offset}")
            timestamp = int(row["timestamp_ms"])
            price = float(row["close"])
            if timestamp <= 0 or not math.isfinite(price) or price <= 0.0:
                raise ValueError(f"invalid sealed observation: {window_id}:{offset}")
            timestamps.append(timestamp)
            prices.append(price)
        if any(second - first != 60_000 for first, second in zip(timestamps, timestamps[1:])):
            raise ValueError(f"non-contiguous sealed minute data: {window_id}")
        window_metadata = by_metadata_id[window_id]
        if (
            window_metadata.get("source_month") != month
            or window_metadata.get("stratum") != stratum
            or int(window_metadata.get("score_start_timestamp_ms", -1))
            != timestamps[BURN_IN_MINUTES]
        ):
            raise ValueError(f"sealed metadata identity mismatch: {window_id}")
        realized_range = max(prices[BURN_IN_MINUTES:]) / min(
            prices[BURN_IN_MINUTES:]
        ) - 1.0
        if not math.isclose(
            float(window_metadata.get("score_range_fraction", math.nan)),
            realized_range,
            rel_tol=1e-12,
            abs_tol=1e-15,
        ):
            raise ValueError(f"sealed metadata range mismatch: {window_id}")
        windows[window_id] = NativeHistoricalWindow(
            name=window_id,
            source_month=month,
            stratum=stratum,
            prices=tuple(prices),
            burn_in_minutes=BURN_IN_MINUTES,
        )
    return lock, windows, hashes


def sealed_jobs(
    profiles: Mapping[str, NativeProfile],
    windows: Mapping[str, NativeHistoricalWindow],
) -> list[dict[str, object]]:
    if set(profiles) != set(LOCKED_SYSTEMS):
        raise ValueError("sealed replay requires exactly three locked profiles")
    if len(windows) != EXPECTED_WINDOWS:
        raise ValueError(f"sealed replay requires {EXPECTED_WINDOWS} windows")
    jobs: list[dict[str, object]] = []
    for window in sorted(windows.values(), key=lambda item: item.name):
        prices = window.prices[::PRIMARY_STEP_MINUTES]
        score_start = window.burn_in_minutes // PRIMARY_STEP_MINUTES
        for flow in FLOW_REGIMES:
            for seed in PRIMARY_SEEDS:
                for routing_mode in SEALED_ROUTES:
                    for system in LOCKED_SYSTEMS:
                        jobs.append(
                            _job(
                                profiles[system],
                                path_name=f"sealed_historical_{window.name}",
                                path_class="sealed_historical",
                                prices=prices,
                                score_start_step=score_start,
                                step_minutes=PRIMARY_STEP_MINUTES,
                                flow=dict(flow),
                                seed=seed,
                                routing_mode=routing_mode,
                                pool_value=POOL_VALUE,
                                source_month=window.source_month,
                                stratum=window.stratum,
                                analysis_slice="sealed_post_lock_validation",
                            )
                        )
    if len(jobs) != EXPECTED_SEALED_ROWS:
        raise AssertionError(
            f"sealed job count={len(jobs)}, expected={EXPECTED_SEALED_ROWS}"
        )
    return jobs


def _result_key(row: Mapping[str, object]) -> tuple[object, ...]:
    return (
        str(row["system"]),
        str(row["profile_id"]),
        str(row["path"]),
        str(row["flow_regime"]),
        int(row["seed"]),
        str(row["routing_mode"]),
    )


def _validate_result_cross(
    rows: Sequence[dict[str, object]],
    jobs: Sequence[dict[str, object]],
) -> None:
    expected = {
        (
            job["profile"].system,
            job["profile"].profile_id,
            job["path_name"],
            job["flow"]["flow_regime"],
            int(job["seed"]),
            job["routing_mode"],
        )
        for job in jobs
    }
    actual = {_result_key(row) for row in rows}
    if len(rows) != EXPECTED_SEALED_ROWS or len(actual) != len(rows):
        raise AssertionError("sealed result rows are incomplete or duplicated")
    if actual != expected:
        raise AssertionError(
            f"sealed result cross mismatch: missing={len(expected-actual)}, "
            f"extra={len(actual-expected)}"
        )
    for row in rows:
        if row.get("analysis_slice") != "sealed_post_lock_validation":
            raise AssertionError("sealed result escaped its isolated analysis slice")
        for field in DECISION_FIELDS:
            value = float(row[field])
            if not math.isfinite(value):
                raise AssertionError(f"non-finite sealed metric: {field}")


def _paired_rows(rows: Sequence[dict[str, object]]) -> list[dict[str, object]]:
    keys = (
        "path",
        "source_month",
        "stratum",
        "flow_regime",
        "seed",
        "routing_mode",
        "initial_pool_value",
        "step_minutes",
        "match_basis",
    )
    grouped: dict[tuple[object, ...], dict[str, dict[str, object]]] = {}
    for row in rows:
        grouped.setdefault(tuple(row[key] for key in keys), {})[
            str(row["system"])
        ] = row
    if len(grouped) != EXPECTED_BASE_CELLS:
        raise AssertionError("sealed paired base-cell count mismatch")
    output: list[dict[str, object]] = []
    for key, systems in grouped.items():
        if set(systems) != set(LOCKED_SYSTEMS):
            raise AssertionError(f"incomplete sealed comparison cell: {key}")
        for left_system, right_system, comparison in PAIR_SPECS:
            left = systems[left_system]
            right = systems[right_system]
            output.append(
                {
                    **{field: value for field, value in zip(keys, key)},
                    "comparison": comparison,
                    "left_system": left_system,
                    "right_system": right_system,
                    "left_minus_right_net_lp_return_after_protocol_fee_bps": (
                        float(left["net_lp_return_after_protocol_fee_bps"])
                        - float(right["net_lp_return_after_protocol_fee_bps"])
                    ),
                    "left_minus_right_gross_net_lp_return_bps": (
                        float(left["net_lp_return_vs_hold_bps"])
                        - float(right["net_lp_return_vs_hold_bps"])
                    ),
                    "left_minus_right_selected_execution_cost_bps": (
                        float(left["selected_execution_cost_bps"])
                        - float(right["selected_execution_cost_bps"])
                    ),
                    "left_minus_right_captured_volume_fraction": (
                        float(left["captured_user_volume_fraction"])
                        - float(right["captured_user_volume_fraction"])
                    ),
                    "left_minus_right_depth_degradation_fraction": (
                        float(left["depth_degradation_fraction"])
                        - float(right["depth_degradation_fraction"])
                    ),
                    "left_minus_right_marginal_tracking_stale_fraction": (
                        float(left["marginal_tracking_stale_fraction"])
                        - float(right["marginal_tracking_stale_fraction"])
                    ),
                }
            )
    if len(output) != EXPECTED_PAIRED_ROWS:
        raise AssertionError("sealed paired row count mismatch")
    return output


def _percentile(values: Sequence[float], probability: float) -> float:
    ordered = sorted(values)
    if not ordered:
        raise ValueError("cannot summarize an empty sealed metric")
    position = (len(ordered) - 1) * probability
    low = math.floor(position)
    high = math.ceil(position)
    if low == high:
        return ordered[low]
    weight = position - low
    return ordered[low] * (1.0 - weight) + ordered[high] * weight


def _summarize(
    rows: Sequence[dict[str, object]],
    paired: Sequence[dict[str, object]],
    *,
    lock: Mapping[str, object],
    input_hashes: Mapping[str, str],
) -> dict[str, object]:
    by_system: dict[str, dict[str, float | int]] = {}
    for system in LOCKED_SYSTEMS:
        system_rows = [row for row in rows if row["system"] == system]
        if len(system_rows) != EXPECTED_BASE_CELLS:
            raise AssertionError(f"sealed system row count mismatch: {system}")
        returns = [
            float(row["net_lp_return_after_protocol_fee_bps"])
            for row in system_rows
        ]
        costs = [float(row["selected_execution_cost_bps"]) for row in system_rows]
        captures = [
            float(row["captured_user_volume_fraction"]) for row in system_rows
        ]
        stale = [
            float(row["marginal_tracking_stale_fraction"]) for row in system_rows
        ]
        depth = [float(row["depth_degradation_fraction"]) for row in system_rows]
        by_system[system] = {
            "rows": len(system_rows),
            "median_net_lp_return_after_protocol_fee_bps": statistics.median(
                returns
            ),
            "p10_net_lp_return_after_protocol_fee_bps": _percentile(returns, 0.10),
            "median_selected_execution_cost_bps": statistics.median(costs),
            "p90_selected_execution_cost_bps": _percentile(costs, 0.90),
            "median_captured_volume_fraction": statistics.median(captures),
            "median_marginal_tracking_stale_fraction": statistics.median(stale),
            "p90_marginal_tracking_stale_fraction": _percentile(stale, 0.90),
            "median_depth_degradation_fraction": statistics.median(depth),
        }

    comparisons: dict[str, dict[str, float | int | str]] = {}
    for _, _, comparison in PAIR_SPECS:
        cells = [row for row in paired if row["comparison"] == comparison]
        deltas = [
            float(row["left_minus_right_net_lp_return_after_protocol_fee_bps"])
            for row in cells
        ]
        execution = [
            float(row["left_minus_right_selected_execution_cost_bps"])
            for row in cells
        ]
        stale = [
            float(row["left_minus_right_marginal_tracking_stale_fraction"])
            for row in cells
        ]
        comparisons[comparison] = {
            "rows": len(cells),
            "orientation": "left minus right; diagnostics only",
            "median_net_lp_return_delta_bps": statistics.median(deltas),
            "p10_net_lp_return_delta_bps": _percentile(deltas, 0.10),
            "left_win_fraction": sum(value > 0.0 for value in deltas) / len(deltas),
            "median_execution_cost_delta_bps": statistics.median(execution),
            "p90_marginal_tracking_stale_delta_fraction": _percentile(stale, 0.90),
        }

    maximum_accounting_error = max(
        abs(float(row["accounting_identity_error"])) for row in rows
    )
    external_reads = sum(int(row["controller_external_price_reads"]) for row in rows)
    capped = sum(int(row["arbitrage_capped_event_count"]) for row in rows)
    numerical = sum(
        int(row["arbitrage_numerical_failure_count"]) for row in rows
    )
    if maximum_accounting_error > 1e-5:
        raise AssertionError("sealed accounting identity error exceeds 1e-5")
    if external_reads != 0:
        raise AssertionError("sealed controller read the external reference price")
    if capped != 0 or numerical != 0:
        raise AssertionError("sealed replay contains arbitrage convergence failures")

    profiles = lock["locked_profiles"]
    return {
        "schema_version": SEALED_OUTPUT_SCHEMA_VERSION,
        "status": "complete",
        "purpose": "post-lock historical diagnostics for already-selected profiles",
        "selection_performed": False,
        "feeds_profile_selection": False,
        "winner_or_recommendation_emitted": False,
        "profile_lock": {
            "fingerprint": lock["profile_lock_fingerprint"],
            "locked_at_utc": lock["locked_at_utc"],
            "profiles": {
                system: {
                    "profile_id": profiles[system]["profile_id"],
                    "description": profiles[system]["description"],
                }
                for system in LOCKED_SYSTEMS
            },
        },
        "input_hashes": dict(input_hashes),
        "matrix": {
            "months": list(SEALED_MONTHS),
            "strata": list(SEALED_STRATA),
            "flows": [flow["flow_regime"] for flow in FLOW_REGIMES],
            "seeds": list(PRIMARY_SEEDS),
            "routes": list(SEALED_ROUTES),
            "windows": EXPECTED_WINDOWS,
            "base_cells": EXPECTED_BASE_CELLS,
            "result_rows": len(rows),
            "paired_rows": len(paired),
        },
        "locked_system_diagnostics": by_system,
        "paired_diagnostics": comparisons,
        "validation": {
            "expected_result_rows": EXPECTED_SEALED_ROWS,
            "expected_paired_rows": EXPECTED_PAIRED_ROWS,
            "all_decision_metrics_finite": True,
            "maximum_accounting_identity_error": maximum_accounting_error,
            "controller_external_price_reads": external_reads,
            "arbitrage_capped_event_count": capped,
            "arbitrage_numerical_failure_count": numerical,
            "input_hashes_verified": True,
            "profile_lock_verified": True,
            "selection_isolation_verified": True,
        },
        "interpretation_limit": (
            "prices are sealed historical observations; flow, routing, and "
            "arbitrage remain modeled; these diagnostics do not re-select profiles"
        ),
    }


def _write_csv(path: Path, rows: Sequence[dict[str, object]]) -> None:
    if not rows:
        raise ValueError(f"no sealed rows for {path}")
    fieldnames: list[str] = []
    seen: set[str] = set()
    for row in rows:
        for field in row:
            if field not in seen:
                fieldnames.append(field)
                seen.add(field)
    with path.open("w", newline="") as target:
        writer = csv.DictWriter(target, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(rows)


def _write_outputs_atomically(
    rows: Sequence[dict[str, object]],
    paired: Sequence[dict[str, object]],
    summary: dict[str, object],
    *,
    output_dir: Path = SEALED_OUTPUT_DIR,
) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix=".sealed-validation-stage-", dir=output_dir
    ) as temporary:
        stage = Path(temporary)
        results_path = stage / "sealed_native_results.csv"
        paired_path = stage / "sealed_native_paired.csv"
        summary_path = stage / "sealed_native_summary.json"
        _write_csv(results_path, rows)
        _write_csv(paired_path, paired)
        summary["output_hashes"] = {
            "sealed_native_results_sha256": sha256_path(results_path),
            "sealed_native_paired_sha256": sha256_path(paired_path),
        }
        summary_path.write_text(
            json.dumps(summary, indent=2, allow_nan=False) + "\n"
        )
        for staged in (results_path, paired_path, summary_path):
            staged.replace(output_dir / staged.name)


def run_sealed_validation(*, workers: int) -> dict[str, object]:
    lock, windows, input_hashes = load_and_verify_sealed_history()
    _verify_locked_matrix_protocol(lock)
    profiles = reconstruct_locked_profiles(lock)
    jobs = sealed_jobs(profiles, windows)
    rows = _run_jobs(jobs, workers, "sealed post-lock native replay")
    _validate_result_cross(rows, jobs)
    paired = _paired_rows(rows)
    summary = _summarize(
        rows,
        paired,
        lock=lock,
        input_hashes=input_hashes,
    )
    _write_outputs_atomically(rows, paired, summary)
    return summary


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument(
        "--materialize-lock",
        action="store_true",
        help=(
            "freeze canonical synthetic-selected Curve/DH2/CPMM profiles before "
            "sealed history is downloaded"
        ),
    )
    parser.add_argument(
        "--locked-at-utc",
        help="optional explicit ISO-8601 lock timestamp for reproducible ceremonies",
    )
    args = parser.parse_args()
    if args.workers <= 0:
        parser.error("--workers must be positive")
    if args.locked_at_utc and not args.materialize_lock:
        parser.error("--locked-at-utc requires --materialize-lock")
    if args.materialize_lock:
        lock = materialize_profile_lock(locked_at_utc=args.locked_at_utc)
        print(json.dumps(lock, indent=2, allow_nan=False))
        return
    summary = run_sealed_validation(workers=args.workers)
    print(json.dumps(summary, indent=2, allow_nan=False))


if __name__ == "__main__":
    main()
