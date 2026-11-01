#!/usr/bin/env python3
"""Freeze stratified SOLUSDT paths for the native-controller benchmark.

The generated benchmark file contains only deterministic 36-hour slices: a
24-hour controller burn-in followed by a 12-hour scoring window.  Results can
therefore be reproduced offline after this one-time data preparation step.
"""

from __future__ import annotations

import csv
import hashlib
import io
import json
import statistics
import urllib.request
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent
DATA = ROOT / "data"
MONTHS = ("2024-03", "2024-08", "2024-12", "2025-01", "2025-04")
BASE_URL = "https://data.binance.vision/data/spot/monthly/klines/SOLUSDT/1m"
BURN_IN_MINUTES = 24 * 60
SCORE_MINUTES = 12 * 60
SLICE_MINUTES = BURN_IN_MINUTES + SCORE_MINUTES
START_STRIDE_MINUTES = 12 * 60


def download(url: str) -> bytes:
    request = urllib.request.Request(
        url,
        headers={"User-Agent": "dusk-native-controller-research/1"},
    )
    with urllib.request.urlopen(request, timeout=90) as response:
        return response.read()


def parse_archive(payload: bytes, month: str) -> list[dict[str, str | int]]:
    rows: list[dict[str, str | int]] = []
    with zipfile.ZipFile(io.BytesIO(payload)) as archive:
        name = archive.namelist()[0]
        with archive.open(name) as source:
            reader = csv.reader(io.TextIOWrapper(source))
            for item in reader:
                timestamp = int(item[0])
                if timestamp > 10**14:
                    timestamp //= 1000
                rows.append(
                    {
                        "timestamp_ms": timestamp,
                        "close": item[4],
                        "quote_volume": item[7],
                        "trades": int(item[8]),
                        "source_month": month,
                    }
                )
    return rows


def score_range(rows: list[dict[str, str | int]], score_start: int) -> float:
    prices = [
        float(row["close"])
        for row in rows[score_start : score_start + SCORE_MINUTES]
    ]
    return max(prices) / min(prices) - 1.0


def overlaps(first: int, second: int) -> bool:
    first_start = first - BURN_IN_MINUTES
    second_start = second - BURN_IN_MINUTES
    return not (
        first_start + SLICE_MINUTES <= second_start
        or second_start + SLICE_MINUTES <= first_start
    )


def choose_strata(rows: list[dict[str, str | int]]) -> list[tuple[str, int, float]]:
    candidates = [
        (start, score_range(rows, start))
        for start in range(
            BURN_IN_MINUTES,
            len(rows) - SCORE_MINUTES + 1,
            START_STRIDE_MINUTES,
        )
    ]
    if len(candidates) < 6:
        raise ValueError("insufficient non-overlapping history candidates")

    selected: list[tuple[str, int, float]] = []
    stress = max(candidates, key=lambda item: (item[1], -item[0]))
    selected.append(("stress", *stress))
    remaining = [item for item in candidates if not overlaps(item[0], stress[0])]

    calm = min(remaining, key=lambda item: (item[1], item[0]))
    selected.append(("calm", *calm))
    remaining = [item for item in remaining if not overlaps(item[0], calm[0])]

    median_range = statistics.median(item[1] for item in remaining)
    typical = min(
        remaining,
        key=lambda item: (abs(item[1] - median_range), item[0]),
    )
    selected.append(("typical", *typical))
    return selected


def main() -> None:
    DATA.mkdir(parents=True, exist_ok=True)
    output_rows: list[dict[str, str | int | float]] = []
    sources = []
    windows = []

    for month in MONTHS:
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
        for stratum, score_start, realized_range in choose_strata(month_rows):
            window_id = f"{month}_{stratum}"
            full_start = score_start - BURN_IN_MINUTES
            selected = month_rows[full_start : score_start + SCORE_MINUTES]
            if len(selected) != SLICE_MINUTES:
                raise AssertionError("native history slice length mismatch")
            for offset, row in enumerate(selected):
                output_rows.append(
                    {
                        "window_id": window_id,
                        "stratum": stratum,
                        "phase": "burn_in" if offset < BURN_IN_MINUTES else "score",
                        "offset_minute": offset,
                        **row,
                    }
                )
            windows.append(
                {
                    "window_id": window_id,
                    "stratum": stratum,
                    "source_month": month,
                    "score_start_timestamp_ms": month_rows[score_start]["timestamp_ms"],
                    "score_range_fraction": realized_range,
                }
            )

    output = DATA / "solusdt_native_windows.csv"
    with output.open("w", newline="") as target:
        writer = csv.DictWriter(target, fieldnames=list(output_rows[0]))
        writer.writeheader()
        writer.writerows(output_rows)

    metadata = {
        "dataset": "SOLUSDT one-minute spot klines",
        "proxy_note": "USDT is a liquid reference-price proxy for SOL/USDC.",
        "selection": (
            "Three non-overlapping 36-hour slices per source month: lowest, "
            "median-nearest, and highest 12-hour close-price range among "
            "12-hour-grid candidates; each scored slice has 24 hours of burn-in."
        ),
        "burn_in_minutes": BURN_IN_MINUTES,
        "score_minutes": SCORE_MINUTES,
        "rows": len(output_rows),
        "windows": windows,
        "snapshot_sha256": hashlib.sha256(output.read_bytes()).hexdigest(),
        "sources": sources,
    }
    (DATA / "native_history_metadata.json").write_text(
        json.dumps(metadata, indent=2) + "\n"
    )
    print(json.dumps(metadata, indent=2))


if __name__ == "__main__":
    main()
