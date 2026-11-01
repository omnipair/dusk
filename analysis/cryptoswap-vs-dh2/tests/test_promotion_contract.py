import csv
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import native_benchmark as benchmark
import promote_controller_profiles as promotion


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def row_digest(row: dict[str, str]) -> str:
    return hashlib.sha256(
        json.dumps(row, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def payload_fingerprint(payload: dict[str, object]) -> str:
    unsigned = dict(payload)
    unsigned.pop("promotion_fingerprint", None)
    return hashlib.sha256(
        json.dumps(
            unsigned,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode()
    ).hexdigest()


class PromotionConsumptionContractTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.summary_relative = (
            "results/curve_controller_search/full/summary.json"
        )
        self.summary_path = self.root / self.summary_relative
        self.grid_dir = self.summary_path.parent / "grid"
        self.grid_dir.mkdir(parents=True)
        self.fingerprint = "a" * 64

        source_fields = benchmark.PROMOTION_SOURCE_FIELDS["curve_native_v2_1"]
        self.source_rows = []
        for index in range(150):
            profile_id = f"curve_grid_c{index // 15:02d}_f{index % 15:02d}"
            row = {
                "profile_id": profile_id,
                "description": f"searched policy {index}",
                "hard_valid": "True" if index < 5 else "False",
                "independent_curve_service_relaxation": str(index / 1000.0),
                "minimum_route_selection_utility": str(100.0 - index),
                "mean_route_selection_utility": str(200.0 - index),
            }
            row.update(
                {
                    field: str(index + field_index / 100.0 + 1.0)
                    for field_index, field in enumerate(source_fields)
                }
            )
            self.source_rows.append(row)

        profiles_path = self.grid_dir / "profiles.csv"
        with profiles_path.open("w", newline="") as target:
            writer = csv.DictWriter(
                target, fieldnames=list(self.source_rows[0])
            )
            writer.writeheader()
            writer.writerows(self.source_rows)
        rows_path = self.grid_dir / "rows.csv"
        rows_path.write_text("profile_id,value\ncurve_grid_c00_f00,1\n")

        winner_id = self.source_rows[0]["profile_id"]
        manifest = {
            "status": "curve_controller_search_stage_complete",
            "fingerprint": self.fingerprint,
            "profiles": 150,
            "cells_per_profile": 128,
            "rows": 19_200,
            "selected_profile_ids": [winner_id],
            "content_hashes": {
                "profiles.csv": sha256(profiles_path),
                "rows.csv": sha256(rows_path),
            },
        }
        manifest_path = self.grid_dir / "manifest.json"
        manifest_path.write_text(json.dumps(manifest))
        summary = {
            "status": "curve_controller_search_complete",
            "mode": "full",
            "decision_grade_search": True,
            "fingerprint": self.fingerprint,
            "winner": {"profile_id": winner_id, "hard_valid": True},
        }
        self.summary_path.write_text(json.dumps(summary))

        promoted_rows = []
        for rank, source_row in enumerate(self.source_rows[:5], start=1):
            promoted = {
                "profile_id": source_row["profile_id"],
                "description": source_row["description"],
                "selected_winner": rank == 1,
                "source_rank": rank,
                "source_profile_row_sha256": row_digest(source_row),
                "model": {"test": rank},
                "config": {"test": rank},
            }
            promoted.update(
                {field: float(source_row[field]) for field in source_fields}
            )
            promoted_rows.append(promoted)
        self.payload = {
            "schema_version": benchmark.PROMOTION_SCHEMA_VERSION,
            "system": "curve_native_v2_1",
            "winner_profile_id": winner_id,
            "search_source": {
                "summary_path": self.summary_relative,
                "summary_sha256": sha256(self.summary_path),
                "stage_manifest_path": str(manifest_path.relative_to(self.root)),
                "stage_manifest_sha256": sha256(manifest_path),
                "profiles_path": str(profiles_path.relative_to(self.root)),
                "profiles_sha256": sha256(profiles_path),
                "fingerprint": self.fingerprint,
            },
            "profiles": promoted_rows,
        }
        self.promotion_path = self.root / "data/curve_controller_profiles.json"
        self.promotion_path.parent.mkdir()
        self._write_payload()

    def tearDown(self):
        self.temporary.cleanup()

    def _write_payload(self):
        self.payload["promotion_fingerprint"] = payload_fingerprint(self.payload)
        self.promotion_path.write_text(json.dumps(self.payload))

    def _load(self):
        with mock.patch.object(benchmark, "ROOT", self.root), mock.patch.object(
            promotion,
            "current_search_fingerprint",
            return_value=self.fingerprint,
        ):
            return benchmark._load_promotion_payload(
                self.promotion_path,
                system="curve_native_v2_1",
                expected_search_path=self.summary_relative,
                expected_status="curve_controller_search_complete",
            )

    def test_exact_source_bound_top_five_loads(self):
        _, rows = self._load()
        self.assertEqual(len(rows), 5)
        self.assertEqual(rows[0]["profile_id"], self.source_rows[0]["profile_id"])

    def test_joint_flattened_and_nested_mutation_is_rejected(self):
        self.payload["profiles"][0]["mid_fee_bps"] += 1.0
        self.payload["profiles"][0]["model"]["test"] = 999
        self._write_payload()
        with self.assertRaisesRegex(ValueError, "differs from its searched row"):
            self._load()

    def test_stale_current_engine_fingerprint_is_rejected(self):
        with mock.patch.object(benchmark, "ROOT", self.root), mock.patch.object(
            promotion,
            "current_search_fingerprint",
            return_value="b" * 64,
        ):
            with self.assertRaisesRegex(ValueError, "stale for the current engine"):
                benchmark._load_promotion_payload(
                    self.promotion_path,
                    system="curve_native_v2_1",
                    expected_search_path=self.summary_relative,
                    expected_status="curve_controller_search_complete",
                )

    def test_arbitrary_searched_policy_cannot_replace_ranked_shortlist(self):
        replacement = self.source_rows[20]
        row = self.payload["profiles"][-1]
        row["profile_id"] = replacement["profile_id"]
        row["description"] = replacement["description"]
        row["source_profile_row_sha256"] = row_digest(replacement)
        for field in benchmark.PROMOTION_SOURCE_FIELDS["curve_native_v2_1"]:
            row[field] = float(replacement[field])
        self._write_payload()
        with self.assertRaisesRegex(ValueError, "exact top-five"):
            self._load()


if __name__ == "__main__":
    unittest.main()
