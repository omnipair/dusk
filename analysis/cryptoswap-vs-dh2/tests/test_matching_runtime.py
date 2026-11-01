from pathlib import Path
import pickle
import sys
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import benchmark
import comparison
from comparison import declared_parameter_bounds, reserves_at_price, service_profile
from models import Cpmm, CryptoSwap, CurrentDh2, LogPotentialDh2, with_zero_fees


def legacy_service_profile(model, prices, trade_sizes):
    """The pre-batching implementation, retained only as a test oracle."""

    rows = []
    for price in prices:
        for trade_bps in trade_sizes:
            for direction in ("x_to_y", "y_to_x"):
                zero_fee = with_zero_fees(model)
                x, y = reserves_at_price(zero_fee, price)
                notional = comparison.POOL_VALUE * trade_bps / 10_000.0
                if direction == "x_to_y":
                    common_input = notional / price
                    quote = zero_fee.quote(
                        x,
                        y,
                        common_input,
                        direction,
                        apply_fee=False,
                        include_marginals=False,
                    )
                    execution_price = quote.amount_out / common_input
                    impact = 1.0 - execution_price / price
                else:
                    quote = zero_fee.quote(
                        x,
                        y,
                        notional,
                        direction,
                        apply_fee=False,
                        include_marginals=False,
                    )
                    execution_price = notional / quote.amount_out
                    impact = execution_price / price - 1.0
                rows.append((price, trade_bps, direction, max(impact * 10_000.0, 1e-9)))
    return rows


class ServiceProfileBatchingTests(unittest.TestCase):
    def test_batched_profile_is_bit_for_bit_equal_to_pre_batching_quotes(self):
        prices = (0.80, 1.0, 1.25)
        trade_sizes = (1, 100, 1_000)
        models = (
            Cpmm(),
            CryptoSwap(amplification=400.0, gamma=0.001),
            CurrentDh2(peak_depth=200.0, fade_scale=0.1),
            LogPotentialDh2(depth_multiplier=20.0, half_width_bps=2_000.0),
        )
        for model in models:
            with self.subTest(model=type(model).__name__):
                legacy = legacy_service_profile(model, prices, trade_sizes)
                batched = service_profile(model, prices, trade_sizes)
                self.assertEqual(len(batched), len(legacy))
                for actual, expected in zip(batched, legacy):
                    price, trade_bps, direction, impact = expected
                    self.assertEqual(actual.normalized_price, price)
                    self.assertEqual(actual.trade_bps, trade_bps)
                    self.assertEqual(actual.direction, direction)
                    self.assertEqual(actual.impact_bps.hex(), impact.hex())

    def test_profile_solves_the_starting_invariant_once_per_price(self):
        original = Cpmm.invariant

        def counted(model, x, y):
            return original(model, x, y)

        prices = (0.9, 1.0, 1.1)
        with patch(
            "comparison.with_zero_fees",
            wraps=comparison.with_zero_fees,
        ) as zero_fees, patch(
            "comparison._reserves_at_price_for_zero_fee",
            wraps=comparison._reserves_at_price_for_zero_fee,
        ) as reserves, patch.object(
            Cpmm,
            "invariant",
            autospec=True,
            side_effect=counted,
        ) as invariant:
            rows = service_profile(Cpmm(), prices, (1, 10, 100))
        self.assertEqual(len(rows), len(prices) * 3 * 2)
        self.assertEqual(zero_fees.call_count, 1)
        self.assertEqual(reserves.call_count, len(prices))
        self.assertEqual(invariant.call_count, len(prices))


class MatchingProcessTests(unittest.TestCase):
    def test_worker_task_and_payload_are_pickleable(self):
        payload = (
            benchmark._run_matching_iso_task,
            ("equal_execution", "curve", 42.0),
        )
        restored_function, restored_task = pickle.loads(pickle.dumps(payload))
        self.assertIs(restored_function, benchmark._run_matching_iso_task)
        self.assertEqual(restored_task, payload[1])

    def test_worker_uses_declared_domains_and_basis_metrics(self):
        calls = []

        def fake_iso(
            constructor,
            first_values,
            second_bounds,
            target,
            metric,
            objective,
            *,
            diagnostics,
        ):
            model = constructor(first_values[0], second_bounds[0])
            calls.append(
                (
                    model,
                    tuple(first_values),
                    second_bounds,
                    target,
                    metric,
                    objective,
                )
            )
            diagnostics["sentinel"] = type(model).__name__
            return model

        families = {
            "curve": CryptoSwap,
            "current_dh2": CurrentDh2,
            "proposed_dh2": LogPotentialDh2,
        }
        with patch.object(benchmark, "_iso_target_best", side_effect=fake_iso):
            for basis in benchmark.MATCHING_BASIS_ORDER:
                for family, model_type in families.items():
                    result_basis, result_family, model, diagnostics = (
                        benchmark._run_matching_iso_task((basis, family, 42.0))
                    )
                    self.assertEqual((result_basis, result_family), (basis, family))
                    self.assertIsInstance(model, model_type)
                    self.assertEqual(diagnostics["sentinel"], model_type.__name__)

        self.assertEqual(len(calls), 6)
        for model, first_values, second_bounds, target, metric, objective in calls:
            first_bounds, expected_second_bounds = declared_parameter_bounds(model)
            self.assertEqual(first_values[0], first_bounds[0])
            self.assertEqual(first_values[-1], first_bounds[1])
            self.assertEqual(len(first_values), comparison.ISO_FIRST_GRID_POINTS)
            self.assertEqual(second_bounds, expected_second_bounds)
            self.assertEqual(target, 42.0)
            basis = (
                "equal_execution"
                if metric is comparison.execution_index_bps
                else "equal_jump_risk"
            )
            if basis == "equal_execution":
                self.assertIs(objective, comparison.jump_loss_index_bps)
            else:
                self.assertIs(metric, comparison.jump_loss_index_bps)
                self.assertIs(objective, comparison.execution_index_bps)

    def test_parallel_results_restore_canonical_model_and_diagnostic_order(self):
        executor_instances = []

        class ReverseResultExecutor:
            def __init__(self, *, max_workers):
                self.max_workers = max_workers
                self.tasks = []
                executor_instances.append(self)

            def __enter__(self):
                return self

            def __exit__(self, exc_type, exc, traceback):
                return False

            def map(self, function, tasks):
                del function
                self.tasks = list(tasks)
                results = []
                for basis, family, target in reversed(self.tasks):
                    if family == "curve":
                        model = CryptoSwap()
                    elif family == "current_dh2":
                        model = CurrentDh2()
                    else:
                        model = LogPotentialDh2()
                    results.append(
                        (basis, family, model, {"task": f"{basis}/{family}", "target": target})
                    )
                return results

        with patch.object(benchmark, "ProcessPoolExecutor", ReverseResultExecutor):
            (
                equal_execution,
                equal_risk,
                execution_diagnostics,
                risk_diagnostics,
            ) = benchmark._parallel_matching_iso_models(12.5, 34.5)

        executor = executor_instances[0]
        self.assertEqual(executor.max_workers, 6)
        self.assertEqual(
            executor.tasks,
            [
                (basis, family, 12.5 if basis == "equal_execution" else 34.5)
                for basis in benchmark.MATCHING_BASIS_ORDER
                for family in benchmark.MATCHING_FAMILY_ORDER
            ],
        )
        self.assertEqual(
            tuple(type(model) for model in equal_execution),
            (CryptoSwap, CurrentDh2, LogPotentialDh2),
        )
        self.assertEqual(
            tuple(type(model) for model in equal_risk),
            (CryptoSwap, CurrentDh2, LogPotentialDh2),
        )
        self.assertEqual(
            tuple(execution_diagnostics),
            benchmark.MATCHING_FAMILY_ORDER,
        )
        self.assertEqual(tuple(risk_diagnostics), benchmark.MATCHING_FAMILY_ORDER)
        self.assertTrue(
            all(
                row["task"].startswith("equal_execution/")
                for row in execution_diagnostics.values()
            )
        )
        self.assertTrue(
            all(
                row["task"].startswith("equal_jump_risk/")
                for row in risk_diagnostics.values()
            )
        )


if __name__ == "__main__":
    unittest.main()
