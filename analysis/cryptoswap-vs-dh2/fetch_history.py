#!/usr/bin/env python3
"""Fetch and freeze official Binance SOLUSDT one-minute candles.

The benchmark itself never calls this script. The committed snapshot is enough
to reproduce every result. Source archives are official public Binance market
data. USDT is used as the liquid quote proxy for USDC; this limitation is stated
in the report.
"""

from __future__ import annotations

import csv
import hashlib
import io
import json
import urllib.request
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent
DATA = ROOT / "data"
MONTHS = ("2024-03", "2024-08", "2024-12", "2025-01", "2025-04")
BASE_URL = "https://data.binance.vision/data/spot/monthly/klines/SOLUSDT/1m"
WINDOW_MINUTES = 720


def download(url: str) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": "dusk-research/1"})
    with urllib.request.urlopen(request, timeout=60) as response:
        return response.read()


def main() -> None:
    DATA.mkdir(parents=True, exist_ok=True)
    rows: list[dict[str, str | int]] = []
    sources = []
    for month in MONTHS:
        url = f"{BASE_URL}/SOLUSDT-1m-{month}.zip"
        payload = download(url)
        digest = hashlib.sha256(payload).hexdigest()
        sources.append({"url": url, "sha256": digest, "bytes": len(payload)})
        with zipfile.ZipFile(io.BytesIO(payload)) as archive:
            name = archive.namelist()[0]
            with archive.open(name) as source:
                reader = csv.reader(io.TextIOWrapper(source))
                for item in reader:
                    timestamp = int(item[0])
                    # Binance archives moved from milliseconds to microseconds.
                    if timestamp > 10**14:
                        timestamp //= 1000
                    rows.append(
                        {
                            "timestamp_ms": timestamp,
                            "open": item[1],
                            "high": item[2],
                            "low": item[3],
                            "close": item[4],
                            "base_volume": item[5],
                            "quote_volume": item[7],
                            "trades": int(item[8]),
                            "source_month": month,
                        }
                    )

    rows.sort(key=lambda row: int(row["timestamp_ms"]))
    selected: list[dict[str, str | int]] = []
    for month in MONTHS:
        month_rows = [row for row in rows if row["source_month"] == month]
        if len(month_rows) < WINDOW_MINUTES:
            raise ValueError(f"insufficient rows for {month}")
        best_start = max(
            range(0, len(month_rows) - WINDOW_MINUTES + 1, 60),
            key=lambda start: max(float(row["close"]) for row in month_rows[start : start + WINDOW_MINUTES])
            / min(float(row["close"]) for row in month_rows[start : start + WINDOW_MINUTES]),
        )
        selected.extend(month_rows[best_start : best_start + WINDOW_MINUTES])

    output = DATA / "solusdt_1m.csv"
    with output.open("w", newline="") as target:
        writer = csv.DictWriter(target, fieldnames=list(selected[0]))
        writer.writeheader()
        writer.writerows(selected)

    snapshot_hash = hashlib.sha256(output.read_bytes()).hexdigest()
    metadata = {
        "dataset": "SOLUSDT one-minute spot klines",
        "proxy_note": "USDT reference is used as a liquid proxy for SOL/USDC.",
        "selection": "highest high/low-range 720-minute window per source month, starts evaluated hourly",
        "rows": len(selected),
        "snapshot_sha256": snapshot_hash,
        "sources": sources,
    }
    (DATA / "history_metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(json.dumps(metadata, indent=2))


if __name__ == "__main__":
    main()
