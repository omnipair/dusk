import ast
from dataclasses import asdict
import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import fetch_native_history
import fetch_sealed_native_history as fetch_sealed
import final_report_evidence as final_evidence
import native_sealed_validation as sealed_validation
import sealed_protocol as sealed
from curve_native import CurveNativeConfig
from economics import default_controlled_config
from models import CryptoSwap, CurrentDh2
from native_benchmark import FLOW_REGIMES, PRIMARY_SEEDS, NativeProfile
from native_sealed_validation import (
    EXPECTED_BASE_CELLS,
    EXPECTED_PAIRED_ROWS,
    EXPECTED_SEALED_ROWS,
    EXPECTED_WINDOWS,
    build_profile_lock_payload,
    reconstruct_locked_profiles,
    sealed_jobs,
)
from scenarios import NativeHistoricalWindow
from sealed_protocol import (
    EXPECTED_SEARCH_CELLS_PER_PROFILE,
    EXPECTED_SEARCH_PROFILES,
    EXPECTED_SEARCH_ROWS,
    LOCKED_SYSTEMS,
    LOCK_SCHEMA_VERSION,
    SEALED_ENGINE_FILES,
    SEALED_MONTHS,
    SEALED_ROUTES,
    SEALED_STRATA,
    SEARCH_SOURCE_PATHS,
    engine_hashes,
    profile_lock_fingerprint,
    search_source_bindings,
    sha256_path,
    verify_postlock_hashes,
    verify_profile_lock,
)


def dusk_search_summary() -> dict[str, object]:
    return {
        "status": "controller_search_complete",
        "mode": "full",
        "initial_profiles": EXPECTED_SEARCH_PROFILES,
        "complete_matrix_evaluated_for_every_initial_profile": True,
        "decision_grade_search": True,
        "stage_counts": {
            "grid": {
                "profiles": EXPECTED_SEARCH_PROFILES,
                "cells_per_profile": EXPECTED_SEARCH_CELLS_PER_PROFILE,
                "rows": EXPECTED_SEARCH_ROWS,
                "selected": 1,
                "exhaustive": True,
            },
            "local": {
                "profiles": 9,
                "cells_per_profile": EXPECTED_SEARCH_CELLS_PER_PROFILE,
                "rows": 9 * EXPECTED_SEARCH_CELLS_PER_PROFILE,
                "selected": 1,
                "exhaustive": True,
            },
        },
        "fingerprints": {"grid": "1" * 64, "local": "2" * 64},
        "winner": {
            "profile_id": "grid_c00_f00",
            "description": "Dusk winner",
            "hard_valid": True,
            "config": {"center_ema_half_life_minutes": 30.0},
        },
    }


def curve_search_summary() -> dict[str, object]:
    return {
        "status": "curve_controller_search_complete",
        "mode": "full",
        "initial_profiles": EXPECTED_SEARCH_PROFILES,
        "exact_profile_budget": EXPECTED_SEARCH_PROFILES,
        "cells_per_profile": EXPECTED_SEARCH_CELLS_PER_PROFILE,
        "rows": EXPECTED_SEARCH_ROWS,
        "complete_matrix_evaluated_for_every_profile": True,
        "decision_grade_search": True,
        "fingerprint": "3" * 64,
        "winner": {
            "profile_id": "curve_c00_f00",
            "description": "Curve winner",
            "hard_valid": True,
            "full_config": {
                "model": {"amplification": 2.0},
                "controller": {"ema_half_life_minutes": 30.0},
            },
        },
    }


def cpmm_search_summary() -> dict[str, object]:
    return {
        "status": "cpmm_controller_search_complete",
        "mode": "full",
        "initial_profiles": EXPECTED_SEARCH_PROFILES,
        "exact_profile_budget": EXPECTED_SEARCH_PROFILES,
        "cells_per_profile": EXPECTED_SEARCH_CELLS_PER_PROFILE,
        "rows": EXPECTED_SEARCH_ROWS,
        "complete_matrix_evaluated_for_every_profile": True,
        "decision_grade_search": True,
        "fingerprint": "8" * 64,
        "winner": {
            "profile_id": "grid_c00_f00",
            "description": "CPMM winner",
            "hard_valid": True,
            "full_config": {
                "model": {"peak_depth": 0.0, "fade_scale": 0.0},
                "controller": {"center_ema_half_life_minutes": 30.0},
            },
        },
    }


def write_search_summaries(root: Path) -> dict[str, Path]:
    summaries = {
        "dusk_controller_search": dusk_search_summary(),
        "curve_controller_search": curve_search_summary(),
        "cpmm_controller_search": cpmm_search_summary(),
    }
    paths = {}
    for name, relative in SEARCH_SOURCE_PATHS.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(summaries[name]))
        paths[name] = path
    return paths


def minimal_search_sources() -> dict[str, object]:
    return {
        "dusk_controller_search": {
            "path": SEARCH_SOURCE_PATHS["dusk_controller_search"],
            "sha256": "4" * 64,
            "status": "controller_search_complete",
            "mode": "full",
            "decision_grade_search": True,
            "complete_matrix": True,
            "matrix": {
                "profiles": EXPECTED_SEARCH_PROFILES,
                "cells_per_profile": EXPECTED_SEARCH_CELLS_PER_PROFILE,
                "rows": EXPECTED_SEARCH_ROWS,
            },
            "local_matrix": {
                "profiles": 9,
                "cells_per_profile": EXPECTED_SEARCH_CELLS_PER_PROFILE,
                "rows": 9 * EXPECTED_SEARCH_CELLS_PER_PROFILE,
            },
            "search_fingerprint": {"grid": "1" * 64, "local": "2" * 64},
            "winner": {
                "profile_id": "grid_c00_f00",
                "description": "Dusk winner",
                "full_config": {"center_ema_half_life_minutes": 30.0},
            },
        },
        "curve_controller_search": {
            "path": SEARCH_SOURCE_PATHS["curve_controller_search"],
            "sha256": "5" * 64,
            "status": "curve_controller_search_complete",
            "mode": "full",
            "decision_grade_search": True,
            "complete_matrix": True,
            "matrix": {
                "profiles": EXPECTED_SEARCH_PROFILES,
                "cells_per_profile": EXPECTED_SEARCH_CELLS_PER_PROFILE,
                "rows": EXPECTED_SEARCH_ROWS,
            },
            "search_fingerprint": "3" * 64,
            "winner": {
                "profile_id": "curve_c00_f00",
                "description": "Curve winner",
                "full_config": {
                    "model": {"amplification": 2.0},
                    "controller": {"ema_half_life_minutes": 30.0},
                },
            },
        },
        "cpmm_controller_search": {
            "path": SEARCH_SOURCE_PATHS["cpmm_controller_search"],
            "sha256": "9" * 64,
            "status": "cpmm_controller_search_complete",
            "mode": "full",
            "decision_grade_search": True,
            "complete_matrix": True,
            "matrix": {
                "profiles": EXPECTED_SEARCH_PROFILES,
                "cells_per_profile": EXPECTED_SEARCH_CELLS_PER_PROFILE,
                "rows": EXPECTED_SEARCH_ROWS,
            },
            "search_fingerprint": "8" * 64,
            "winner": {
                "profile_id": "grid_c00_f00",
                "description": "CPMM winner",
                "full_config": {
                    "model": {"peak_depth": 0.0, "fade_scale": 0.0},
                    "controller": {
                        "center_ema_half_life_minutes": 30.0
                    },
                },
            },
        },
    }


def minimal_lock() -> dict[str, object]:
    payload: dict[str, object] = {
        "schema_version": LOCK_SCHEMA_VERSION,
        "locked_at_utc": "2026-07-27T00:00:00Z",
        "selection_source": {
            "path": "results/native_summary.json",
            "sha256": sha256_path(ROOT / "results" / "native_summary.json"),
        },
        "search_sources": minimal_search_sources(),
        "selection_binding": {
            system: {
                "profile_id": "locked",
                "description": f"locked {system}",
                "model_fingerprint": "a" * 64,
                "config_fingerprint": "b" * 64,
                "promotion_source": {
                    "path": sealed_validation.PROMOTION_SOURCE_PATHS[system],
                    "sha256": "c" * 64,
                    "promotion_fingerprint": "d" * 64,
                },
            }
            for system in LOCKED_SYSTEMS
        },
        "locked_profiles": {
            system: {
                "system": system,
                "profile_id": "locked",
                "description": f"locked {system}",
                "model": {"value": 1},
                "config": {"value": 1},
            }
            for system in LOCKED_SYSTEMS
        },
        "dataset_protocol": {
            "source_months": list(SEALED_MONTHS),
            "strata": list(SEALED_STRATA),
        },
        "matrix_protocol": {
            "systems": list(LOCKED_SYSTEMS),
            "routing_modes": list(SEALED_ROUTES),
        },
        "engine_hashes": engine_hashes(),
    }
    payload["profile_lock_fingerprint"] = profile_lock_fingerprint(payload)
    return payload


def canonical_lock_profiles() -> dict[tuple[str, str], NativeProfile]:
    config = default_controlled_config()
    profiles = (
        NativeProfile(
            "curve_native_v2_1",
            "curve_c00_f00",
            CryptoSwap(),
            CurveNativeConfig(),
            "Curve winner",
        ),
        NativeProfile(
            "dusk_native",
            "grid_c00_f00",
            CurrentDh2(),
            config,
            "Dusk winner",
        ),
        NativeProfile(
            "dusk_cpmm",
            "grid_c00_f00",
            CurrentDh2(peak_depth=0.0, fade_scale=0.0),
            config,
            "CPMM winner",
        ),
    )
    return {(profile.system, profile.profile_id): profile for profile in profiles}


def canonical_native_summary(
    profiles: dict[tuple[str, str], NativeProfile],
) -> dict[str, object]:
    sources = minimal_search_sources()
    search_name = {
        "curve_native_v2_1": "curve_controller_search",
        "dusk_native": "dusk_controller_search",
        "dusk_cpmm": "cpmm_controller_search",
    }
    promotions = {}
    selection = {}
    promotion_hashes = {
        "curve_native_v2_1": ("a" * 64, "b" * 64),
        "dusk_native": ("c" * 64, "d" * 64),
        "dusk_cpmm": ("e" * 64, "f" * 64),
    }
    for (system, profile_id), profile in profiles.items():
        source = sources[search_name[system]]
        fingerprint = source["search_fingerprint"]
        if system == "dusk_native":
            fingerprint = fingerprint["grid"]
        promotions[system] = {
            "search_summary_sha256": source["sha256"],
            "search_fingerprint": fingerprint,
            "promotion_sha256": promotion_hashes[system][0],
            "promotion_fingerprint": promotion_hashes[system][1],
            "search_winner_profile_id": profile_id,
            "canonical_locked_profile_id": profile_id,
            "promoted_profiles": 5,
        }
        selection[system] = {
            "profile_id": profile_id,
            "description": profile.description,
        }
    return {
        "status": "complete",
        "profile_selection": selection,
        "validation": {
            "canonical_output": True,
            "post_run_audit": {"status": "passed"},
            "search_promotion_audit": {
                "status": "passed",
                "symmetric_concentrated_search": True,
                "independent_cpmm_search": True,
                "promotions": promotions,
            },
        },
    }


def promoted_rows(
    profiles: dict[tuple[str, str], NativeProfile],
) -> dict[str, dict[str, object]]:
    return {
        system: {
            "profile_id": profile.profile_id,
            "description": profile.description,
            "selected_winner": True,
            "source_rank": 1,
            "model": asdict(profile.model),
            "config": asdict(profile.config),
        }
        for (system, _), profile in profiles.items()
    }


class SealedProtocolTests(unittest.TestCase):
    def test_lock_schema_binds_three_independent_search_families(self):
        self.assertEqual(LOCK_SCHEMA_VERSION, 3)
        self.assertEqual(
            set(SEARCH_SOURCE_PATHS),
            {
                "curve_controller_search",
                "dusk_controller_search",
                "cpmm_controller_search",
            },
        )

    def test_lock_creation_binds_exact_promoted_model_and_config(self):
        profiles = canonical_lock_profiles()
        rows = promoted_rows(profiles)
        summary = canonical_native_summary(profiles)
        sources = minimal_search_sources()
        stable_engine_hashes = {name: "6" * 64 for name in SEALED_ENGINE_FILES}

        def load_promoted(system, profile_id, *, expected_sha256, expected_fingerprint):
            self.assertEqual(rows[system]["profile_id"], profile_id)
            return rows[system], {
                "path": sealed_validation.PROMOTION_SOURCE_PATHS[system],
                "sha256": str(expected_sha256),
                "promotion_fingerprint": str(expected_fingerprint),
            }

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            summary_path = root / "results" / "native_summary.json"
            summary_path.parent.mkdir(parents=True)
            summary_path.write_text(json.dumps(summary))
            with patch.object(sealed_validation, "ROOT", root), patch.object(
                sealed_validation, "_profile_inventory", return_value=profiles
            ), patch.object(
                sealed_validation, "search_source_bindings", return_value=sources
            ), patch.object(
                sealed_validation, "engine_hashes", return_value=stable_engine_hashes
            ), patch.object(
                sealed_validation, "_load_promoted_profile", side_effect=load_promoted
            ):
                lock = build_profile_lock_payload(
                    summary_path=summary_path,
                    locked_at_utc="2026-07-27T00:00:00Z",
                )

        for system in LOCKED_SYSTEMS:
            profile = profiles[(system, lock["locked_profiles"][system]["profile_id"])]
            self.assertEqual(lock["locked_profiles"][system]["model"], asdict(profile.model))
            self.assertEqual(lock["locked_profiles"][system]["config"], asdict(profile.config))
            self.assertEqual(
                lock["selection_binding"][system]["model_fingerprint"],
                sealed_validation._payload_fingerprint(asdict(profile.model)),
            )
            self.assertEqual(
                lock["selection_binding"][system]["config_fingerprint"],
                sealed_validation._payload_fingerprint(asdict(profile.config)),
            )

    def test_lock_creation_rejects_promoted_model_or_config_drift(self):
        for drift_field in ("model", "config"):
            with self.subTest(drift_field=drift_field), tempfile.TemporaryDirectory() as temporary:
                profiles = canonical_lock_profiles()
                rows = promoted_rows(profiles)
                rows["dusk_native"] = json.loads(json.dumps(rows["dusk_native"]))
                if drift_field == "model":
                    rows["dusk_native"]["model"]["peak_depth"] += 1.0
                else:
                    rows["dusk_native"]["config"]["adjustment_step"] += 0.001
                summary = canonical_native_summary(profiles)
                root = Path(temporary)
                summary_path = root / "results" / "native_summary.json"
                summary_path.parent.mkdir(parents=True)
                summary_path.write_text(json.dumps(summary))

                def load_promoted(system, profile_id, *, expected_sha256, expected_fingerprint):
                    return rows[system], {
                        "path": sealed_validation.PROMOTION_SOURCE_PATHS[system],
                        "sha256": str(expected_sha256),
                        "promotion_fingerprint": str(expected_fingerprint),
                    }

                with patch.object(sealed_validation, "ROOT", root), patch.object(
                    sealed_validation, "_profile_inventory", return_value=profiles
                ), patch.object(
                    sealed_validation,
                    "search_source_bindings",
                    return_value=minimal_search_sources(),
                ), patch.object(
                    sealed_validation,
                    "engine_hashes",
                    return_value={name: "6" * 64 for name in SEALED_ENGINE_FILES},
                ), patch.object(
                    sealed_validation,
                    "_load_promoted_profile",
                    side_effect=load_promoted,
                ):
                    with self.assertRaisesRegex(ValueError, "promoted model/config"):
                        build_profile_lock_payload(
                            summary_path=summary_path,
                            locked_at_utc="2026-07-27T00:00:00Z",
                        )

    def test_reconstruction_rejects_resigned_locked_config_drift(self):
        profiles = canonical_lock_profiles()
        rows = promoted_rows(profiles)
        summary = canonical_native_summary(profiles)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            summary_path = root / "results" / "native_summary.json"
            summary_path.parent.mkdir(parents=True)
            summary_path.write_text(json.dumps(summary))

            def load_promoted(system, profile_id, *, expected_sha256, expected_fingerprint):
                return rows[system], {
                    "path": sealed_validation.PROMOTION_SOURCE_PATHS[system],
                    "sha256": str(expected_sha256),
                    "promotion_fingerprint": str(expected_fingerprint),
                }

            with patch.object(sealed_validation, "ROOT", root), patch.object(
                sealed_validation, "_profile_inventory", return_value=profiles
            ), patch.object(
                sealed_validation,
                "search_source_bindings",
                return_value=minimal_search_sources(),
            ), patch.object(
                sealed_validation,
                "engine_hashes",
                return_value={name: "6" * 64 for name in SEALED_ENGINE_FILES},
            ), patch.object(
                sealed_validation,
                "_load_promoted_profile",
                side_effect=load_promoted,
            ):
                lock = build_profile_lock_payload(
                    summary_path=summary_path,
                    locked_at_utc="2026-07-27T00:00:00Z",
                )
                lock["locked_profiles"]["curve_native_v2_1"]["config"][
                    "ema_half_life_minutes"
                ] += 1.0
                lock["profile_lock_fingerprint"] = profile_lock_fingerprint(lock)
                with self.assertRaisesRegex(
                    ValueError, "model/config binding mismatch"
                ):
                    reconstruct_locked_profiles(lock)

    def test_month_rule_is_mechanical_and_fixed_before_download(self):
        self.assertEqual(
            SEALED_MONTHS,
            ("2025-05", "2025-08", "2025-11", "2026-02", "2026-05"),
        )
        month_numbers = [
            int(year) * 12 + int(month) - 1
            for year, month in (value.split("-") for value in SEALED_MONTHS)
        ]
        self.assertEqual(
            [right - left for left, right in zip(month_numbers, month_numbers[1:])],
            [3, 3, 3, 3],
        )
        self.assertIs(fetch_sealed.choose_strata, fetch_native_history.choose_strata)

    def test_sha256_and_postlock_sidecars_fail_closed_on_tampering(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lock = root / "lock.json"
            history = root / "history.csv"
            metadata = root / "metadata.json"
            manifest = root / "manifest.json"
            lock.write_bytes(b"locked-profile")
            history.write_bytes(b"window_id,close\na,150\n")
            metadata.write_text(
                json.dumps(
                    {
                        "sealed_profile_lock_sha256": sha256_path(lock),
                        "snapshot_sha256": sha256_path(history),
                    }
                )
            )
            manifest.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "profile_lock_sha256": sha256_path(lock),
                        "history_csv_sha256": sha256_path(history),
                        "history_metadata_sha256": sha256_path(metadata),
                    }
                )
            )
            hashes = verify_postlock_hashes(
                lock_path=lock,
                csv_path=history,
                metadata_path=metadata,
                manifest_path=manifest,
            )
            self.assertEqual(
                hashes["history_csv_sha256"],
                hashlib.sha256(history.read_bytes()).hexdigest(),
            )
            history.write_bytes(history.read_bytes() + b"b,151\n")
            with self.assertRaisesRegex(ValueError, "history_csv_sha256"):
                verify_postlock_hashes(
                    lock_path=lock,
                    csv_path=history,
                    metadata_path=metadata,
                    manifest_path=manifest,
                )

    def test_lock_content_tampering_is_rejected_before_environment_reads(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "sealed_profile_lock.json"
            payload = minimal_lock()
            path.write_text(json.dumps(payload))
            verify_profile_lock(path, verify_environment=False)
            payload["locked_profiles"]["dusk_native"]["profile_id"] = "tampered"
            path.write_text(json.dumps(payload))
            with self.assertRaisesRegex(ValueError, "fingerprint mismatch"):
                verify_profile_lock(path, verify_environment=False)

    def test_selection_source_hash_mismatch_is_rejected_even_with_resigned_lock(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "sealed_profile_lock.json"
            payload = minimal_lock()
            payload["selection_source"]["sha256"] = "0" * 64
            payload["profile_lock_fingerprint"] = profile_lock_fingerprint(payload)
            path.write_text(json.dumps(payload))
            with self.assertRaisesRegex(ValueError, "selection source changed"):
                verify_profile_lock(path, verify_environment=True)

    def test_search_bindings_require_all_three_complete_full_150_by_128_matrices(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(
            sealed, "ROOT", Path(temporary)
        ):
            paths = write_search_summaries(Path(temporary))
            bindings = search_source_bindings(paths=paths)
            self.assertEqual(set(bindings), set(SEARCH_SOURCE_PATHS))
            self.assertEqual(
                bindings["dusk_controller_search"]["matrix"],
                {
                    "profiles": 150,
                    "cells_per_profile": 128,
                    "rows": 19_200,
                },
            )
            self.assertEqual(
                bindings["curve_controller_search"]["winner"]["full_config"],
                curve_search_summary()["winner"]["full_config"],
            )
            self.assertEqual(
                bindings["cpmm_controller_search"]["winner"]["full_config"],
                cpmm_search_summary()["winner"]["full_config"],
            )

            invalid = dusk_search_summary()
            invalid["stage_counts"]["grid"]["rows"] = 19_199
            paths["dusk_controller_search"].write_text(json.dumps(invalid))
            with self.assertRaisesRegex(ValueError, "row count must be exactly 19200"):
                search_source_bindings(paths=paths)

    def test_missing_or_non_decision_grade_search_binding_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "sealed_profile_lock.json"
            payload = minimal_lock()
            del payload["search_sources"]["cpmm_controller_search"]
            payload["profile_lock_fingerprint"] = profile_lock_fingerprint(payload)
            path.write_text(json.dumps(payload))
            with self.assertRaisesRegex(ValueError, "bind exactly"):
                verify_profile_lock(path, verify_environment=False)

            payload = minimal_lock()
            payload["search_sources"]["dusk_controller_search"][
                "decision_grade_search"
            ] = False
            payload["profile_lock_fingerprint"] = profile_lock_fingerprint(payload)
            path.write_text(json.dumps(payload))
            with self.assertRaisesRegex(ValueError, "not decision-grade"):
                verify_profile_lock(path, verify_environment=False)

    def test_search_source_or_bound_fields_changing_after_lock_fails_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            native_summary = root / "results" / "native_summary.json"
            native_summary.parent.mkdir(parents=True, exist_ok=True)
            native_summary.write_text('{"status":"complete"}')
            paths = write_search_summaries(root)
            stable_engine_hashes = {name: "6" * 64 for name in SEALED_ENGINE_FILES}
            payload = minimal_lock()
            cpmm_payload = minimal_lock()
            with patch.object(sealed, "ROOT", root), patch.object(
                sealed, "engine_hashes", return_value=stable_engine_hashes
            ):
                payload["selection_source"]["sha256"] = sha256_path(native_summary)
                payload["search_sources"] = search_source_bindings(paths=paths)
                payload["engine_hashes"] = stable_engine_hashes
                payload["profile_lock_fingerprint"] = profile_lock_fingerprint(payload)
                lock_path = root / "sealed_profile_lock.json"
                lock_path.write_text(json.dumps(payload))
                verify_profile_lock(lock_path, verify_environment=True)

                changed = curve_search_summary()
                changed["winner"]["description"] = "changed after lock"
                paths["curve_controller_search"].write_text(json.dumps(changed))
                with self.assertRaisesRegex(ValueError, "search source changed"):
                    verify_profile_lock(lock_path, verify_environment=True)

                paths["curve_controller_search"].write_text(
                    json.dumps(curve_search_summary())
                )
                payload["search_sources"]["curve_controller_search"][
                    "search_fingerprint"
                ] = "7" * 64
                payload["profile_lock_fingerprint"] = profile_lock_fingerprint(payload)
                lock_path.write_text(json.dumps(payload))
                with self.assertRaisesRegex(ValueError, "search source changed"):
                    verify_profile_lock(lock_path, verify_environment=True)

                payload = cpmm_payload
                payload["selection_source"]["sha256"] = sha256_path(native_summary)
                payload["search_sources"] = search_source_bindings(paths=paths)
                payload["engine_hashes"] = stable_engine_hashes
                payload["profile_lock_fingerprint"] = profile_lock_fingerprint(payload)
                lock_path.write_text(json.dumps(payload))
                changed_cpmm = cpmm_search_summary()
                changed_cpmm["winner"]["description"] = "changed CPMM winner"
                paths["cpmm_controller_search"].write_text(json.dumps(changed_cpmm))
                with self.assertRaisesRegex(ValueError, "search source changed"):
                    verify_profile_lock(lock_path, verify_environment=True)


class SealedMatrixTests(unittest.TestCase):
    def test_summary_uses_maximum_absolute_accounting_error(self):
        rows = []
        for accounting_error in (2.5165718398056924e-09, -2.0809096668017446e-07):
            rows.append(
                {
                    "system": "dusk_native",
                    "net_lp_return_after_protocol_fee_bps": 0.0,
                    "selected_execution_cost_bps": 0.0,
                    "captured_user_volume_fraction": 1.0,
                    "marginal_tracking_stale_fraction": 0.0,
                    "depth_degradation_fraction": 0.0,
                    "accounting_identity_error": accounting_error,
                    "controller_external_price_reads": 0,
                    "arbitrage_capped_event_count": 0,
                    "arbitrage_numerical_failure_count": 0,
                }
            )
        lock = {
            "profile_lock_fingerprint": "a" * 64,
            "locked_at_utc": "2026-07-29T00:00:00Z",
            "locked_profiles": {
                "dusk_native": {
                    "profile_id": "locked",
                    "description": "locked DH2",
                }
            },
        }
        with patch.object(sealed_validation, "LOCKED_SYSTEMS", ("dusk_native",)), patch.object(
            sealed_validation, "EXPECTED_BASE_CELLS", len(rows)
        ), patch.object(sealed_validation, "PAIR_SPECS", ()):
            summary = sealed_validation._summarize(
                rows,
                [],
                lock=lock,
                input_hashes={},
            )

        self.assertEqual(
            summary["validation"]["maximum_accounting_identity_error"],
            2.0809096668017446e-07,
        )

    def test_final_report_matrix_contract_matches_sealed_replay(self):
        self.assertEqual(
            final_evidence._expected_sealed_matrix(),
            {
                "months": list(SEALED_MONTHS),
                "strata": list(SEALED_STRATA),
                "flows": [flow["flow_regime"] for flow in FLOW_REGIMES],
                "seeds": list(PRIMARY_SEEDS),
                "routes": list(SEALED_ROUTES),
                "windows": EXPECTED_WINDOWS,
                "base_cells": EXPECTED_BASE_CELLS,
                "result_rows": EXPECTED_SEALED_ROWS,
                "paired_rows": EXPECTED_PAIRED_ROWS,
            },
        )

    def test_expected_full_cross_and_counts(self):
        config = default_controlled_config()
        profiles = {
            "curve_native_v2_1": NativeProfile(
                "curve_native_v2_1",
                "curve_locked",
                CryptoSwap(),
                CurveNativeConfig(),
                "locked Curve",
            ),
            "dusk_native": NativeProfile(
                "dusk_native",
                "dh2_locked",
                CurrentDh2(),
                config,
                "locked DH2",
            ),
            "dusk_cpmm": NativeProfile(
                "dusk_cpmm",
                "cpmm_locked",
                CurrentDh2(peak_depth=0.0, fade_scale=0.0),
                config,
                "locked CPMM",
            ),
        }
        windows = {}
        for month in SEALED_MONTHS:
            for stratum in SEALED_STRATA:
                name = f"{month}_{stratum}"
                windows[name] = NativeHistoricalWindow(
                    name=name,
                    source_month=month,
                    stratum=stratum,
                    prices=tuple(150.0 for _ in range(2_160)),
                    burn_in_minutes=1_440,
                )
        jobs = sealed_jobs(profiles, windows)
        self.assertEqual(EXPECTED_WINDOWS, 15)
        self.assertEqual(EXPECTED_BASE_CELLS, 240)
        self.assertEqual(EXPECTED_SEALED_ROWS, 720)
        self.assertEqual(EXPECTED_PAIRED_ROWS, 720)
        self.assertEqual(len(jobs), EXPECTED_SEALED_ROWS)
        self.assertEqual(
            {job["profile"].system for job in jobs}, set(LOCKED_SYSTEMS)
        )
        self.assertEqual(
            {job["flow"]["flow_regime"] for job in jobs},
            {flow["flow_regime"] for flow in FLOW_REGIMES},
        )
        self.assertEqual({job["seed"] for job in jobs}, set(PRIMARY_SEEDS))
        self.assertEqual(
            {job["routing_mode"] for job in jobs}, set(SEALED_ROUTES)
        )
        self.assertEqual(
            {job["analysis_slice"] for job in jobs},
            {"sealed_post_lock_validation"},
        )

    def test_selection_and_search_modules_do_not_import_or_read_sealed_artifacts(self):
        for filename in ("native_benchmark.py", "dusk_controller_search.py"):
            path = ROOT / filename
            source = path.read_text()
            tree = ast.parse(source)
            imported_modules = set()
            for node in ast.walk(tree):
                if isinstance(node, ast.Import):
                    imported_modules.update(alias.name for alias in node.names)
                elif isinstance(node, ast.ImportFrom) and node.module:
                    imported_modules.add(node.module)
            with self.subTest(filename=filename):
                self.assertFalse(
                    any(module.startswith("sealed") for module in imported_modules)
                )
                self.assertNotIn("sealed_profile_lock.json", source)
                self.assertNotIn("solusdt_sealed_native_windows.csv", source)
                self.assertNotIn("sealed_native_history_metadata.json", source)


if __name__ == "__main__":
    unittest.main()
