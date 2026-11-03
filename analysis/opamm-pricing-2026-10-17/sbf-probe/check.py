"""Compare each executable SBF quote with an independent 100-digit model."""

from __future__ import annotations

import json
from decimal import Decimal, ROUND_FLOOR, getcontext
from pathlib import Path

getcontext().prec = 100
D = Decimal
C = D(4) / D(5)
P0 = D(100)


def expected_atoms(op: int, pref_nad: int, gross: int, base: int, quote: int, fee: int) -> int:
    x = P0 * D(base)
    y = D(quote)
    pref = D(pref_nad) / D(1_000_000_000)
    a = (pref / P0) if op % 2 == 0 else (pref / P0).sqrt()
    net = gross * (10_000 - fee) // 10_000
    if net == 0:
        return 0
    if op < 2:
        target = (y**C + a * x**C - (y + D(net)) ** C) / a
        out = (x - target ** (D(5) / D(4))) / P0
    else:
        gain = a * ((x + P0 * D(net)) ** C - x**C)
        target = y**C - gain
        out = y - target ** (D(5) / D(4))
    return int(out.to_integral_value(rounding=ROUND_FLOOR))


def main() -> None:
    import sys
    path = Path(__file__).with_name("results-sbf-stress.json" if "--stress" in sys.argv else "results-sbf.json")
    document = json.loads(path.read_text())
    rows = document["rows"]
    gaps = []
    for row in rows:
        expected = expected_atoms(row["op"], int(row["pref_nad"]), int(row["input_atoms"]),
                                  int(row["base_atoms"]), int(row["quote_atoms"]), row["fee_bps"])
        actual = int(row["output_atoms"])
        gap = expected - actual
        if gap < 0:
            raise AssertionError(f"Pool overpaid: {row} expected_floor={expected}")
        row["reference_floor_atoms"] = str(expected)
        row["underquote_atoms"] = gap
        gaps.append(gap)
    result = {"count": len(rows), "max_underquote_atoms": max(gaps),
              "nonzero_underquotes": sum(gap > 0 for gap in gaps),
              "max_overquote_atoms": 0}
    document["summary"] = result
    path.write_text(json.dumps(document, indent=2) + "\n")
    print(result)


if __name__ == "__main__":
    main()
