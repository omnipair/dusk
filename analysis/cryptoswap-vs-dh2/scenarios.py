"""Deterministic synthetic and frozen historical scenarios."""

from __future__ import annotations

import csv
from dataclasses import dataclass
import math
from pathlib import Path
import random


ROOT = Path(__file__).resolve().parent


@dataclass(frozen=True)
class NativeHistoricalWindow:
    name: str
    source_month: str
    stratum: str
    prices: tuple[float, ...]
    burn_in_minutes: int

    @property
    def scoring_prices(self) -> tuple[float, ...]:
        return self.prices[self.burn_in_minutes :]


def synthetic_paths(length: int = 720, seed: int = 7) -> dict[str, list[float]]:
    randomizer = random.Random(seed)
    paths: dict[str, list[float]] = {}
    paths["flat"] = [150.0] * length
    paths["single_jump"] = [150.0 if i < length // 3 else 180.0 for i in range(length)]
    paths["jump_reversal"] = [
        150.0 if i < length // 3 or i >= 2 * length // 3 else 180.0 for i in range(length)
    ]
    paths["trend"] = [150.0 * math.exp(math.log(1.5) * i / (length - 1)) for i in range(length)]
    paths["staircase"] = [150.0 * (1.04 ** (i // 90)) for i in range(length)]
    paths["mean_reversion"] = [150.0 * (1.0 + 0.16 * math.sin(2 * math.pi * i / 180)) for i in range(length)]
    paths["alternating_chop"] = [150.0 * (1.025 if i % 2 else 0.975) for i in range(length)]
    clustered = [150.0]
    volatility = 0.002
    for index in range(1, length):
        if index % 120 == 0:
            volatility = 0.012 if volatility < 0.01 else 0.002
        clustered.append(clustered[-1] * math.exp(randomizer.gauss(0, volatility)))
    paths["volatility_cluster"] = clustered
    return paths


def historical_windows(window: int = 720) -> dict[str, list[float]]:
    path = ROOT / "data" / "solusdt_1m.csv"
    if not path.exists():
        raise FileNotFoundError(f"missing committed history snapshot: {path}")
    by_month: dict[str, list[float]] = {}
    with path.open() as source:
        for row in csv.DictReader(source):
            by_month.setdefault(row["source_month"], []).append(float(row["close"]))
    windows = {}
    for month, prices in sorted(by_month.items()):
        if len(prices) < window:
            continue
        # Pick the highest realized-range window deterministically. This
        # deliberately stress-tests each month rather than pretending to be a
        # random population estimate.
        best_start = max(
            range(0, len(prices) - window + 1, 60),
            key=lambda start: max(prices[start : start + window]) / min(prices[start : start + window]),
        )
        windows[f"historical_{month}"] = prices[best_start : best_start + window]
    return windows


def native_historical_windows() -> dict[str, NativeHistoricalWindow]:
    """Load frozen non-overlapping paths with explicit controller burn-in."""

    path = ROOT / "data" / "solusdt_native_windows.csv"
    if not path.exists():
        raise FileNotFoundError(f"missing native history snapshot: {path}")
    grouped: dict[str, list[dict[str, str]]] = {}
    with path.open() as source:
        for row in csv.DictReader(source):
            grouped.setdefault(row["window_id"], []).append(row)

    windows: dict[str, NativeHistoricalWindow] = {}
    for name, rows in sorted(grouped.items()):
        rows.sort(key=lambda row: int(row["offset_minute"]))
        burn_in = sum(row["phase"] == "burn_in" for row in rows)
        if burn_in <= 0 or burn_in >= len(rows):
            raise ValueError(f"invalid burn-in boundary for {name}")
        windows[name] = NativeHistoricalWindow(
            name=name,
            source_month=rows[0]["source_month"],
            stratum=rows[0]["stratum"],
            prices=tuple(float(row["close"]) for row in rows),
            burn_in_minutes=burn_in,
        )
    return windows
