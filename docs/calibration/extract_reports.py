"""Extract Dusk native calibration CSV records from a successful test log."""

import csv
from pathlib import Path
import sys


def main():
    if len(sys.argv) != 2:
        raise SystemExit("Usage: python3 docs/calibration/extract_reports.py /path/to/test.log")
    log = Path(sys.argv[1]).read_text()
    if "test result: ok." not in log or "test result: FAILED." in log:
        raise SystemExit("Refusing a log without a successful test result or with failed tests")
    output_dir = Path(__file__).resolve().parent
    files = {
        "MARGIN_CALIBRATION": ("total_liquidity", "leverage-entry-candidates.csv"),
        "RECOVERY_CALIBRATION": ("curve_depth", "leverage-partial-recovery.csv"),
        "EMERGENCY_CALIBRATION": ("amplification", "leverage-emergency-paths.csv"),
        "STORED_DEPTH_ENTRY": ("liquidity", "leverage-stored-depth-entry.csv"),
        "STORED_DEPTH_PATH": ("amplification", "leverage-stored-depth-paths.csv"),
    }
    for marker, (first_column, filename) in files.items():
        records = [line.partition(",")[2] for line in log.splitlines() if line.startswith(marker + ",")]
        if not records:
            continue
        parsed = list(csv.reader(records))
        headers = [record for record in parsed if record[0] == first_column]
        if not headers or any(header != headers[0] for header in headers):
            raise SystemExit(f"Missing or inconsistent header for {marker}")
        rows = [record for record in parsed if record[0] != first_column]
        if not rows or any(len(row) != len(headers[0]) for row in rows):
            raise SystemExit(f"Missing or malformed rows for {marker}")
        with (output_dir / filename).open("w", newline="") as output:
            writer = csv.writer(output, lineterminator="\n")
            writer.writerow(headers[0])
            writer.writerows(rows)
        print(f"{filename}: {len(rows)} rows")


if __name__ == "__main__":
    main()
