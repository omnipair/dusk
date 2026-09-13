#!/usr/bin/env python3
"""Generate deterministic appendix data for the Dusk v2 whitepaper.

The script intentionally uses only the Python standard library so it can run in
minimal environments. Values are illustrative sanity checks of equations in the
paper, not market forecasts.
"""

from __future__ import annotations

import csv
import math
from pathlib import Path


ROOT = Path(__file__).resolve().parent
DATA = ROOT / "data"
FIGURES = ROOT / "figures"

NAD = 1.0
TARGET_UTIL = 0.90
STEEPNESS = 4.0
INITIAL_RATE_AT_TARGET = 0.04
MIN_RATE_AT_TARGET = 0.001
MAX_RATE_AT_TARGET = 2.0
ADJUSTMENT_SPEED_PER_YEAR = 20.0
MAX_ADAPTATION_STEP = 0.5


def ensure_dirs() -> None:
    DATA.mkdir(parents=True, exist_ok=True)
    FIGURES.mkdir(parents=True, exist_ok=True)


def utilization_error(util: float) -> float:
    if util <= TARGET_UTIL:
        return (util - TARGET_UTIL) / TARGET_UTIL
    return (util - TARGET_UTIL) / (1.0 - TARGET_UTIL)


def curve_multiplier(error: float) -> float:
    if error >= 0:
        return 1.0 + (STEEPNESS - 1.0) * error
    return 1.0 - (1.0 - 1.0 / STEEPNESS) * abs(error)


def step_rate_at_target(rate_at_target: float, error: float, days: float) -> float:
    exponent = ADJUSTMENT_SPEED_PER_YEAR * error * (days / 365.0)
    exponent = max(-MAX_ADAPTATION_STEP, min(MAX_ADAPTATION_STEP, exponent))
    # Dusk currently uses a bounded linear approximation to exp(exponent).
    factor = max(0.0, 1.0 + exponent)
    return max(MIN_RATE_AT_TARGET, min(MAX_RATE_AT_TARGET, rate_at_target * factor))


def generate_interest_paths() -> list[dict[str, float | str]]:
    rows: list[dict[str, float | str]] = []
    scenarios = {
        "zero_util": 0.0,
        "target_util": TARGET_UTIL,
        "full_util": 1.0,
    }
    for name, util in scenarios.items():
        rate_at_target = INITIAL_RATE_AT_TARGET
        error = utilization_error(util)
        for day in range(0, 31):
            multiplier = curve_multiplier(error)
            rows.append(
                {
                    "scenario": name,
                    "day": day,
                    "utilization": util,
                    "rate_at_target_apr": rate_at_target,
                    "borrow_rate_apr": rate_at_target * multiplier,
                }
            )
            rate_at_target = step_rate_at_target(rate_at_target, error, 1.0)
    return rows


def amount_out(x: float, y: float, dx: float) -> float:
    return dx * y / (x + dx)


def generate_slippage() -> list[dict[str, float | str]]:
    rows: list[dict[str, float | str]] = []
    base_x = 1_000_000.0
    base_y = 1_000_000.0
    for scale in [1.0, 1.5, 2.0, 4.0]:
        x = base_x * scale
        y = base_y * scale
        for trade_bps in [10, 50, 100, 250, 500, 1000]:
            dx = base_x * trade_bps / 10_000.0
            dy = amount_out(x, y, dx)
            execution_price = dy / dx
            spot_price = y / x
            slippage_bps = (1.0 - execution_price / spot_price) * 10_000.0
            rows.append(
                {
                    "depth_scale": scale,
                    "trade_bps_of_initial_depth": trade_bps,
                    "amount_in": dx,
                    "amount_out": dy,
                    "slippage_bps": slippage_bps,
                }
            )
    return rows


def generate_tracking_loss() -> list[dict[str, float]]:
    rows: list[dict[str, float]] = []
    equity = 100.0
    for pct in range(-50, 101, 5):
        ratio = 1.0 + pct / 100.0
        if ratio <= 0:
            continue
        loss = equity * (math.sqrt(ratio) - 1.0) ** 2
        pre_adjustment = equity * abs(math.sqrt(ratio) - 1.0)
        rows.append(
            {
                "price_move_pct": pct,
                "price_ratio": ratio,
                "equity": equity,
                "tracking_loss": loss,
                "closed_form_pre_adjustment": pre_adjustment,
            }
        )
    return rows


def write_csv(path: Path, rows: list[dict[str, object]]) -> None:
    if not rows:
        raise ValueError(f"no rows for {path}")
    with path.open("w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=list(rows[0].keys()))
        writer.writeheader()
        writer.writerows(rows)


def polyline(points: list[tuple[float, float]], x_min: float, x_max: float, y_min: float, y_max: float) -> str:
    width = 640.0
    height = 360.0
    left = 58.0
    top = 28.0
    plot_w = width - 88.0
    plot_h = height - 78.0
    coords = []
    for x, y in points:
        px = left + (x - x_min) / (x_max - x_min) * plot_w
        py = top + (1.0 - (y - y_min) / (y_max - y_min)) * plot_h
        coords.append(f"{px:.2f},{py:.2f}")
    return " ".join(coords)


def write_svg(path: Path, title: str, series: list[tuple[str, list[tuple[float, float]], str]]) -> None:
    x_values = [x for _, pts, _ in series for x, _ in pts]
    y_values = [y for _, pts, _ in series for _, y in pts]
    x_min, x_max = min(x_values), max(x_values)
    y_min, y_max = min(y_values), max(y_values)
    if math.isclose(y_min, y_max):
        y_max = y_min + 1.0
    pad = (y_max - y_min) * 0.08
    y_min = max(0.0, y_min - pad)
    y_max += pad

    lines = [
        '<svg xmlns="http://www.w3.org/2000/svg" width="640" height="360" viewBox="0 0 640 360">',
        '<rect width="640" height="360" fill="white"/>',
        f'<text x="58" y="20" font-family="Arial" font-size="16" font-weight="700">{title}</text>',
        '<line x1="58" y1="310" x2="610" y2="310" stroke="#222" stroke-width="1"/>',
        '<line x1="58" y1="28" x2="58" y2="310" stroke="#222" stroke-width="1"/>',
        f'<text x="58" y="340" font-family="Arial" font-size="11">x: {x_min:g} to {x_max:g}</text>',
        f'<text x="430" y="340" font-family="Arial" font-size="11">y: {y_min:.4g} to {y_max:.4g}</text>',
    ]
    for idx, (name, pts, color) in enumerate(series):
        lines.append(
            f'<polyline fill="none" stroke="{color}" stroke-width="2" points="{polyline(pts, x_min, x_max, y_min, y_max)}"/>'
        )
        y = 48 + idx * 18
        lines.append(f'<line x1="470" y1="{y}" x2="500" y2="{y}" stroke="{color}" stroke-width="2"/>')
        lines.append(f'<text x="506" y="{y+4}" font-family="Arial" font-size="11">{name}</text>')
    lines.append("</svg>")
    path.write_text("\n".join(lines) + "\n")


def generate_svgs(interest: list[dict[str, object]], slippage: list[dict[str, object]], tracking: list[dict[str, object]]) -> None:
    interest_series = []
    for scenario, color in [("zero_util", "#b64242"), ("target_util", "#333333"), ("full_util", "#2f6fb0")]:
        pts = [
            (float(row["day"]), float(row["borrow_rate_apr"]) * 100.0)
            for row in interest
            if row["scenario"] == scenario
        ]
        interest_series.append((scenario, pts, color))
    write_svg(FIGURES / "adaptive_irm.svg", "Adaptive IRM borrow APR (%)", interest_series)

    slip_series = []
    for scale, color in [(1.0, "#b64242"), (2.0, "#2f6fb0"), (4.0, "#2f8f57")]:
        pts = [
            (float(row["trade_bps_of_initial_depth"]), float(row["slippage_bps"]))
            for row in slippage
            if float(row["depth_scale"]) == scale
        ]
        slip_series.append((f"depth {scale:g}x", pts, color))
    write_svg(FIGURES / "slippage_depth.svg", "Slippage vs live depth", slip_series)

    tracking_pts = [
        (float(row["price_move_pct"]), float(row["tracking_loss"]))
        for row in tracking
    ]
    write_svg(FIGURES / "hlp_tracking_loss.svg", "hLP tracking loss, E0=100", [("loss", tracking_pts, "#7048a8")])


def main() -> None:
    ensure_dirs()
    interest = generate_interest_paths()
    slippage = generate_slippage()
    tracking = generate_tracking_loss()

    write_csv(DATA / "adaptive_irm.csv", interest)
    write_csv(DATA / "slippage_depth.csv", slippage)
    write_csv(DATA / "hlp_tracking_loss.csv", tracking)
    generate_svgs(interest, slippage, tracking)

    print("Generated:")
    print(f"  {DATA / 'adaptive_irm.csv'}")
    print(f"  {DATA / 'slippage_depth.csv'}")
    print(f"  {DATA / 'hlp_tracking_loss.csv'}")
    print(f"  {FIGURES / 'adaptive_irm.svg'}")
    print(f"  {FIGURES / 'slippage_depth.svg'}")
    print(f"  {FIGURES / 'hlp_tracking_loss.svg'}")


if __name__ == "__main__":
    main()
