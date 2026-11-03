"""Reproducible, independent paper-rule pricing benchmark on saved public books."""

from __future__ import annotations

import csv
import gzip
import hashlib
import json
import statistics
import time
from dataclasses import dataclass
from pathlib import Path

from model import Pool, RULES

HERE = Path(__file__).resolve().parent
SOURCE = HERE.parent / "oracle-guided-amm-2026-10-16" / "data" / "reference_1m.csv"
FEE = 0.0003
BOOK_FEE = 0.0005
CAPITAL = 1_000_000.0
SIZES = (500.0, 2500.0, 10000.0, 25000.0, 50000.0)


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def make_pool(price: float, rule) -> Pool:
    return Pool(price, CAPITAL / (2 * price), CAPITAL / 2, rule, FEE)


def book_cash(levels, base_quantity: float, buy: bool) -> float | None:
    """Finite displayed levels; quote paid (+fee) or received (-fee)."""
    needed = base_quantity
    cash = 0.0
    for raw_price, raw_quantity in levels:
        take = min(needed, float(raw_quantity))
        cash += take * float(raw_price)
        needed -= take
        if needed <= 1e-10:
            return cash * (1 + BOOK_FEE if buy else 1 - BOOK_FEE)
    return None


@dataclass
class Run:
    pool: Pool
    start_base: float
    start_quote: float
    observations: int = 0
    arbitrages: int = 0
    profit_quote: float = 0.0
    buy_probe_bps_sum: float = 0.0
    sell_probe_bps_sum: float = 0.0

    def step(self, reference: float, mark: float, bids, asks) -> None:
        self.observations += 1
        best = (0.0, "", 0.0)
        for size in SIZES:
            trial = self.pool.fork()
            try:
                base_out = trial.buy_base_exact_in(size, reference)
                proceeds = book_cash(bids, base_out, buy=False)
                if proceeds is not None and proceeds - size > best[0]:
                    best = (proceeds - size, "buy", size)
            except ValueError:
                pass
            base_in = size / mark
            trial = self.pool.fork()
            try:
                quote_out = trial.sell_base_exact_in(base_in, reference)
                cost = book_cash(asks, base_in, buy=True)
                if cost is not None and quote_out - cost > best[0]:
                    best = (quote_out - cost, "sell", base_in)
            except ValueError:
                pass
        if best[0] > 1e-8:
            if best[1] == "buy":
                base_out = self.pool.buy_base_exact_in(best[2], reference)
                realized = book_cash(bids, base_out, buy=False) - best[2]
            else:
                quote_out = self.pool.sell_base_exact_in(best[2], reference)
                realized = quote_out - book_cash(asks, best[2], buy=True)
            assert abs(realized - best[0]) < 1e-7
            self.arbitrages += 1
            self.profit_quote += realized

        # Same non-executed $1,000 probes on the post-arbitrage state.
        buy = self.pool.fork().buy_base_exact_in(1000.0, reference)
        sell_base = 1000.0 / mark
        sell = self.pool.fork().sell_base_exact_in(sell_base, reference)
        self.buy_probe_bps_sum += (1000.0 / (buy * mark) - 1) * 10000
        self.sell_probe_bps_sum += (1 - sell / 1000.0) * 10000

    def result(self, episode: str, feed: str, rule: str, final_mark: float) -> dict:
        hodl = self.start_base * final_mark + self.start_quote
        nav = self.pool.nav(final_mark)
        return {
            "episode": episode,
            "feed": feed,
            "rule": rule,
            "observations": self.observations,
            "arbitrages": self.arbitrages,
            "arb_profit_quote": round(self.profit_quote, 6),
            "lp_nav_quote": round(nav, 6),
            "lp_hodl_shortfall_quote": round(hodl - nav, 6),
            "buy_1k_probe_cost_mean_bps": round(self.buy_probe_bps_sum / self.observations, 4),
            "sell_1k_probe_cost_mean_bps": round(self.sell_probe_bps_sum / self.observations, 4),
            "end_base": round(self.pool.base, 9),
            "end_quote": round(self.pool.quote, 6),
        }


def historical() -> tuple[list[dict], dict]:
    runs = {}
    final_marks = {}
    previous_mid = None
    previous_episode = None
    selected = 0
    rows_seen = 0
    with SOURCE.open(newline="") as stream:
        reader = csv.DictReader(stream)
        first = next(reader)
        book_path = (SOURCE.parent / first["bybit_book_file"]).resolve()
        stream.seek(0)
        reader = csv.DictReader(stream)
        with gzip.open(book_path, "rt") as books:
            for index, (row, raw_line) in enumerate(zip(reader, books)):
                rows_seen += 1
                episode = row["episode"]
                mark = float(row["bybit_mid"])
                final_marks[episode] = mark
                if episode != previous_episode:
                    previous_mid = None
                    previous_episode = episode
                lagged = mark if previous_mid is None else previous_mid
                previous_mid = mark
                if index % 5 != 0 or row["bybit_valid"] != "True":
                    continue
                raw = json.loads(raw_line)
                assert int(raw["decision_timestamp_us"]) == int(row["timestamp_us"])
                selected += 1
                for feed, reference in (("ideal", mark), ("lag60_assumed", lagged)):
                    for rule in RULES:
                        key = episode, feed, rule.name
                        if key not in runs:
                            pool = make_pool(mark, rule)
                            runs[key] = Run(pool, pool.base, pool.quote)
                        runs[key].step(reference, mark, raw["bids"], raw["asks"])
            assert next(books, None) is None, "book file has extra rows"
    assert rows_seen == 30240, f"unexpected input row count: {rows_seen}"
    assert selected == 6048, f"unexpected selected book count: {selected}"
    rows = [run.result(*key, final_marks[key[0]]) for key, run in sorted(runs.items())]
    return rows, {"input_rows": rows_seen, "selected_books": selected, "book_sha256": sha(book_path)}


def static_probes() -> list[dict]:
    result = []
    for rule in RULES:
        for move in (0.0, 0.1):
            pool = make_pool(100.0, rule)
            reference = 100.0 * (1 + move)
            buy = pool.fork().buy_base_exact_in(10000.0, reference)
            sell = pool.fork().sell_base_exact_in(100.0, reference)
            result.append({
                "rule": rule.name,
                "oracle_move_pct": move * 100,
                "marginal_price": round(pool.marginal_price(reference), 8),
                "buy_10k_base_out": round(buy, 9),
                "buy_10k_average_price": round(10000.0 / buy, 8),
                "sell_100_base_quote_out": round(sell, 8),
                "sell_100_base_average_price": round(sell / 100.0, 8),
            })
    return result


def repeated_bias_cycles() -> list[dict]:
    """Greedy base-neutral cycles; reports alternate 4% low and 4% high."""
    rows = []
    for rule in RULES:
        pool = make_pool(100.0, rule)
        starting_quote = pool.quote
        starting_base = pool.base
        total_profit = 0.0
        profitable = 0
        for cycle in range(20):
            best = (0.0, 0.0)
            for size in SIZES:
                base = size / 100.0
                trial = pool.fork()
                try:
                    cost = trial.buy_base_exact_out(base, 96.0)
                    proceeds = trial.sell_base_exact_in(base, 104.0)
                except ValueError:
                    continue
                if proceeds - cost > best[0]:
                    best = (proceeds - cost, base)
            if best[0] <= 1e-8:
                break
            cost = pool.buy_base_exact_out(best[1], 96.0)
            proceeds = pool.sell_base_exact_in(best[1], 104.0)
            total_profit += proceeds - cost
            profitable += 1
            assert abs(pool.base - starting_base) < 1e-8
        rows.append({
            "rule": rule.name,
            "profitable_cycles_out_of_20": profitable,
            "attacker_quote_profit": round(total_profit, 6),
            "lp_quote_loss": round(starting_quote - pool.quote, 6),
            "base_inventory_change": round(pool.base - starting_base, 12),
            "assumed_reports": "96 then 104; true mark fixed at 100",
        })
    return rows


def host_timing() -> list[dict]:
    rows = []
    for rule in RULES:
        samples = []
        for _ in range(5):
            start = time.perf_counter_ns()
            for _ in range(10000):
                make_pool(100.0, rule).buy_base_exact_in(1000.0, 103.0)
            samples.append((time.perf_counter_ns() - start) / 10000)
        rows.append({"rule": rule.name, "median_python_ns_per_quote_including_pool_creation": round(statistics.median(samples))})
    return rows


if __name__ == "__main__":
    historical_rows, source_info = historical()
    report = {
        "model_sha256": sha(HERE / "model.py"),
        "benchmark_sha256": sha(HERE / "benchmark.py"),
        "source_csv_sha256": sha(SOURCE),
        "source": source_info,
        "parameters": {
            "capital_quote": CAPITAL,
            "pool_fee_bps": FEE * 10000,
            "external_book_fee_bps": BOOK_FEE * 10000,
            "arb_sizes_quote": SIZES,
            "book_sampling_minutes": 5,
            "lagged_feed": "previous one-minute Bybit mid; synthetic 60-second reporting delay",
            "historical_trade_flow": "one best positive book-hedged arbitrage per sampled minute; no customer trades",
        },
        "static": static_probes(),
        "historical": historical_rows,
        "repeated_bias": repeated_bias_cycles(),
        "host_timing": host_timing(),
    }
    out = HERE / "results.json"
    out.write_text(json.dumps(report, indent=2) + "\n")
    print(f"wrote {out} with {len(historical_rows)} historical policy rows")
