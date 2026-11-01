#!/usr/bin/env python3
"""Fetch post-lock SOLUSDT windows for sealed native validation.

The month schedule is mechanical and fixed before any archive is downloaded.
The existing native benchmark's deterministic calm/typical/stress rule is
reused exactly.  This script refuses to run without a valid profile lock and
refuses to overwrite an existing sealed dataset.
"""

from __future__ import annotations

import csv
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import tempfile

from fetch_native_history import (
    BASE_URL,
    BURN_IN_MINUTES,
    SCORE_MINUTES,
    SLICE_MINUTES,
    choose_strata,
    download,
    parse_archive,
)
from sealed_protocol import (
    LOCK_PATH,
    SEALED_CSV_PATH,
    SEALED_HISTORY_SCHEMA_VERSION,
    SEALED_MANIFEST_PATH,
    SEALED_METADATA_PATH,
    SEALED_MONTHS,
    SEALED_STRATA,
    canonical_fingerprint,
    profile_lock_fingerprint,
    sha256_path,
    verify_profile_lock,
)


def _utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _write_csv(path: Path, rows: list[dict[str, object]]) -> None:
    if not rows:
        raise ValueError("sealed history cannot be empty")
    with path.open("w", newline="") as target:
        writer = csv.DictWriter(target, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def main() -> None:
    lock = verify_profile_lock(LOCK_PATH, verify_environment=True)
    outputs = (SEALED_CSV_PATH, SEALED_METADATA_PATH, SEALED_MANIFEST_PATH)
    existing = [str(path) for path in outputs if path.exists()]
    if existing:
        raise FileExistsError(
            "sealed history is immutable and already exists: " + ", ".join(existing)
        )

    output_rows: list[dict[str, object]] = []
    sources: list[dict[str, object]] = []
    windows: list[dict[str, object]] = []
    for month in SEALED_MONTHS:
        url = f"{BASE_URL}/SOLUSDT-1m-{month}.zip"
        payload = download(url)
        sources.append(
            {
                "url": url,
                "sha256": hashlib.sha256(payload).hexdigest(),
                "bytes": len(payload),
            }
        )
        month_rows = parse_archive(payload, month)
        selected_strata = choose_strata(month_rows)
        if {row[0] for row in selected_strata} != set(SEALED_STRATA):
            raise AssertionError("sealed stratum selection is incomplete")
        for stratum, score_start, realized_range in selected_strata:
            window_id = f"{month}_{stratum}"
            full_start = score_start - BURN_IN_MINUTES
            selected = month_rows[full_start : score_start + SCORE_MINUTES]
            if len(selected) != SLICE_MINUTES:
                raise AssertionError("sealed native history slice length mismatch")
            for offset, row in enumerate(selected):
                output_rows.append(
                    {
                        "window_id": window_id,
                        "stratum": stratum,
                        "phase": (
                            "burn_in" if offset < BURN_IN_MINUTES else "score"
                        ),
                        "offset_minute": offset,
                        **row,
                    }
                )
            windows.append(
                {
                    "window_id": window_id,
                    "stratum": stratum,
                    "source_month": month,
                    "score_start_timestamp_ms": month_rows[score_start][
                        "timestamp_ms"
                    ],
                    "score_range_fraction": realized_range,
                }
            )

    expected_rows = len(SEALED_MONTHS) * len(SEALED_STRATA) * SLICE_MINUTES
    if len(output_rows) != expected_rows:
        raise AssertionError(
            f"sealed history rows={len(output_rows)}, expected={expected_rows}"
        )

    SEALED_CSV_PATH.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix=".sealed-history-stage-", dir=SEALED_CSV_PATH.parent
    ) as temporary:
        stage = Path(temporary)
        staged_csv = stage / SEALED_CSV_PATH.name
        staged_metadata = stage / SEALED_METADATA_PATH.name
        staged_manifest = stage / SEALED_MANIFEST_PATH.name
        _write_csv(staged_csv, output_rows)
        lock_sha = sha256_path(LOCK_PATH)
        generated_at = _utc_now()
        metadata = {
            "schema_version": SEALED_HISTORY_SCHEMA_VERSION,
            "dataset": "SOLUSDT one-minute spot klines",
            "proxy_note": "USDT is a liquid reference-price proxy for SOL/USDC.",
            "sealed_post_lock": True,
            "generated_at_utc": generated_at,
            "sealed_profile_lock_sha256": lock_sha,
            "sealed_profile_lock_fingerprint": profile_lock_fingerprint(lock),
            "source_months": list(SEALED_MONTHS),
            "selection": (
                "Three non-overlapping 36-hour slices per source month: lowest, "
                "median-nearest, and highest 12-hour close-price range among "
                "12-hour-grid candidates; each scored slice has 24 hours of burn-in."
            ),
            "selection_protocol_fingerprint": canonical_fingerprint(
                lock["dataset_protocol"]
            ),
            "burn_in_minutes": BURN_IN_MINUTES,
            "score_minutes": SCORE_MINUTES,
            "rows": len(output_rows),
            "windows": windows,
            "snapshot_sha256": sha256_path(staged_csv),
            "sources": sources,
        }
        staged_metadata.write_text(json.dumps(metadata, indent=2) + "\n")
        manifest = {
            "schema_version": 1,
            "generated_at_utc": generated_at,
            "profile_lock_sha256": lock_sha,
            "history_csv_path": f"data/{SEALED_CSV_PATH.name}",
            "history_csv_sha256": sha256_path(staged_csv),
            "history_metadata_path": f"data/{SEALED_METADATA_PATH.name}",
            "history_metadata_sha256": sha256_path(staged_metadata),
        }
        staged_manifest.write_text(json.dumps(manifest, indent=2) + "\n")
        for staged, destination in (
            (staged_csv, SEALED_CSV_PATH),
            (staged_metadata, SEALED_METADATA_PATH),
            (staged_manifest, SEALED_MANIFEST_PATH),
        ):
            staged.replace(destination)

    print(json.dumps(metadata, indent=2))


if __name__ == "__main__":
    main()
