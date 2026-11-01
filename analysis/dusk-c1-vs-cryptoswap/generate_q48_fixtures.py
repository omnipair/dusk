#!/usr/bin/env python3
"""Generate deterministic analytical fixtures for the standalone Q48 Rust model."""

from __future__ import annotations

import csv
import math
from pathlib import Path

from models import DuskC1


ROOT = Path(__file__).resolve().parent
OUTPUT = ROOT / "rust-q48" / "tests" / "fixtures.csv"
EXACT_OUT_OUTPUT = ROOT / "rust-q48" / "tests" / "fixtures_exact_out.csv"
NAD = 1_000_000_000
PROFILES = (
    (20.0, 0.05),
    (200.0, 0.005),
    (2_000.0, 0.0005),
)


def main() -> None:
    rows: list[dict[str, object]] = []
    exact_out_rows: list[dict[str, object]] = []
    for peak_depth, fade_scale in PROFILES:
        for bridge_fraction in (0.25, 0.5):
            model = DuskC1(peak_depth, fade_scale, bridge_fraction)
            geometry = model.bridge_geometry()
            v_points = (
                0.0,
                geometry.v_start * 0.5,
                geometry.v_start * 0.99,
                geometry.v_start + geometry.length * 0.01,
                geometry.v_start + geometry.length * 0.5,
                geometry.v_start + geometry.length * 0.99,
                geometry.v_tail * 1.01,
                geometry.v_tail * 2.0,
            )
            for centered_raw in (10**9, 10**12):
                for sign in (-1.0, 1.0):
                    for v in v_points:
                        signed_u = math.copysign(math.asinh(v), sign)
                        x_float, y_float = model.coordinates(2.0, signed_u)
                        x = max(1, round(x_float * centered_raw))
                        y = max(1, round(y_float * centered_raw))
                        for fraction in (1e-5, 1e-3, 0.05):
                            amount_in = max(1, round(x * fraction))
                            x_after = x + amount_in
                            current_v = model.imbalance(float(x), float(y))
                            # Same-side exact-tail moves are integer CPMM:
                            # choose ceil(x*y/x_after), which is equivalent to
                            # flooring exact-input output.  Every crossing uses
                            # the full canonical-invariant contract below.
                            cpmm_remaining = (x * y + x_after - 1) // x_after
                            cpmm_v = model.imbalance(float(x_after), float(cpmm_remaining))
                            same_side = (x - y) * (x_after - cpmm_remaining) > 0
                            if (
                                model.region_for_v(current_v) == "tail"
                                and same_side
                                and model.region_for_v(cpmm_v) == "tail"
                            ):
                                remaining_atom = cpmm_remaining
                            else:
                                # Mirror the integer contract: persist the
                                # smallest invariant atom on the conservative
                                # side, then select the smallest valid reserve.
                                invariant = math.ceil(
                                    math.nextafter(model.invariant(float(x), float(y)), -math.inf)
                                )
                                remaining, _, _ = model.reserve_y(
                                    float(x_after), float(invariant)
                                )
                                remaining_atom = math.ceil(
                                    math.nextafter(remaining, -math.inf)
                                )
                            expected = y - remaining_atom
                            if expected <= 0:
                                continue
                            rows.append(
                                {
                                    "peak_depth_nad": round(peak_depth * NAD),
                                    "fade_scale_nad": round(fade_scale * NAD),
                                    "bridge_numerator": 1 if bridge_fraction == 0.25 else 2,
                                    "reserve_in": x,
                                    "reserve_out": y,
                                    "amount_in": amount_in,
                                    "expected_amount_out_floor": expected,
                                }
                            )

                            amount_out = max(1, round(y * fraction))
                            if amount_out >= y:
                                continue
                            y_after = y - amount_out
                            cpmm_input_reserve = (x * y + y_after - 1) // y_after
                            cpmm_v = model.imbalance(
                                float(cpmm_input_reserve), float(y_after)
                            )
                            same_side = (x - y) * (cpmm_input_reserve - y_after) > 0
                            if (
                                model.region_for_v(current_v) == "tail"
                                and same_side
                                and model.region_for_v(cpmm_v) == "tail"
                            ):
                                input_reserve_atom = cpmm_input_reserve
                            else:
                                invariant = math.ceil(
                                    math.nextafter(
                                        model.invariant(float(x), float(y)), -math.inf
                                    )
                                )
                                input_reserve, _, _ = model.reserve_y(
                                    float(y_after), float(invariant)
                                )
                                input_reserve_atom = math.ceil(
                                    math.nextafter(input_reserve, -math.inf)
                                )
                            expected_input = input_reserve_atom - x
                            if expected_input <= 0:
                                continue
                            exact_out_rows.append(
                                {
                                    "peak_depth_nad": round(peak_depth * NAD),
                                    "fade_scale_nad": round(fade_scale * NAD),
                                    "bridge_numerator": (
                                        1 if bridge_fraction == 0.25 else 2
                                    ),
                                    "reserve_in": x,
                                    "reserve_out": y,
                                    "amount_out": amount_out,
                                    "expected_amount_in_ceil": expected_input,
                                }
                            )

    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    with OUTPUT.open("w", newline="") as destination:
        writer = csv.DictWriter(destination, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    with EXACT_OUT_OUTPUT.open("w", newline="") as destination:
        writer = csv.DictWriter(destination, fieldnames=list(exact_out_rows[0]))
        writer.writeheader()
        writer.writerows(exact_out_rows)
    print(f"wrote {len(rows)} fixtures to {OUTPUT}")
    print(f"wrote {len(exact_out_rows)} fixtures to {EXACT_OUT_OUTPUT}")


if __name__ == "__main__":
    main()
