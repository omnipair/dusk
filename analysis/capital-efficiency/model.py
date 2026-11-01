"""Deterministic Dusk/v1 research model. Python standard library only.

This is a real-valued curve model, not an on-chain transaction simulator.
Dynamic fee effectiveness is a sensitivity input, not an estimated result.
"""
from dataclasses import dataclass
from pathlib import Path
import csv
import json
import math


@dataclass(frozen=True)
class Curve:
    capital: float = 1_000_000.0
    amplification: float = 1.0
    core_bps: int = 0
    fade_bps: int = 0
    center: float = 1.0

    def __post_init__(self):
        if self.capital <= 0 or self.center <= 0 or not 1 <= self.amplification <= 2000:
            raise ValueError("Positive capital/center and amplification in [1, 2000] required")
        if self.amplification == 1:
            if self.core_bps or self.fade_bps:
                raise ValueError("CPMM requires zero widths")
        elif self.core_bps <= 0 or self.fade_bps < 0 or self.core_bps + self.fade_bps > 65535:
            raise ValueError("Invalid concentrated widths")
        if self.tail <= 0:
            raise ValueError("Amplification/width combination leaves no positive tail")

    @property
    def reference_liquidity(self):
        return self.capital / (2 * math.sqrt(self.center))

    @property
    def inverse_width(self):
        core = 1 / math.sqrt(1 + self.core_bps / 10000)
        outer = 1 / math.sqrt(1 + (self.core_bps + self.fade_bps) / 10000)
        return (core + outer) / 2 if self.fade_bps else core

    @property
    def concentrated(self):
        return (self.amplification - 1) * self.reference_liquidity / self.inverse_width

    @property
    def tail(self):
        return self.amplification * self.reference_liquidity - self.concentrated

    @property
    def layers(self):
        if self.amplification == 1:
            return []
        widths = [self.core_bps, self.core_bps + self.fade_bps] if self.fade_bps else [self.core_bps]
        return [(self.concentrated / len(widths), math.sqrt(self.center / (1 + w / 10000)),
                 math.sqrt(self.center * (1 + w / 10000))) for w in widths]

    def inventory(self, price):
        if price <= 0:
            raise ValueError("Price must be positive")
        s = math.sqrt(price)
        base, quote = self.tail / s, self.tail * s
        for liquidity, lower, upper in self.layers:
            clipped = min(upper, max(lower, s))
            base += liquidity * (1 / clipped - 1 / upper)
            quote += liquidity * (clipped - lower)
        return base, quote

    def active_liquidity(self, price):
        s = math.sqrt(price)
        return self.tail + sum(l for l, lo, hi in self.layers if lo < s < hi)

    def nav(self, price):
        x, y = self.inventory(price)
        return price * x + y

    def lvr_daily(self, price, annual_volatility):
        # 0.5 sigma^2 P^2 |dx/dP|; dx/dP = -L_active / (2 P^(3/2)).
        return self.active_liquidity(price) * math.sqrt(price) * annual_volatility**2 / (4 * 365)

    def buy(self, gross_quote, fee_bps=0, start_price=None):
        """One quote-to-base trade with an exogenous constant input fee."""
        if gross_quote < 0 or not 0 <= fee_bps < 10000:
            raise ValueError("Invalid input or fee")
        start = self.center if start_price is None else start_price
        x0, y0 = self.inventory(start)
        target_quote = y0 + gross_quote * (1 - fee_bps / 10000)
        low, high = start, start * 2
        while self.inventory(high)[1] < target_quote:
            high *= 2
        for _ in range(100):
            mid = (low + high) / 2
            if self.inventory(mid)[1] < target_quote:
                low = mid
            else:
                high = mid
        end = (low + high) / 2
        output = x0 - self.inventory(end)[0]
        return {"gross_quote": gross_quote, "base_out": output, "end_price": end,
                "average_cost_bps": (gross_quote / output / start - 1) * 10000 if output > 0 else 0}

    def jump_arbitrage(self, jump, fee_bps=30, lp_fee_share=0.8):
        """One upward external jump, frozen curve, arbitrageur's optimal trade.

        Gross curve shortfall and fee income are separate. LP net shortfall
        equals arbitrageur profit plus non-LP fee allocation. Cash unlimited.
        """
        if jump < 0 or not 0 <= fee_bps < 10000 or not 0 <= lp_fee_share <= 1:
            raise ValueError("Invalid jump/fee/share")
        external = self.center * (1 + jump)
        fee = fee_bps / 10000
        endpoint = max(self.center, external * (1 - fee))
        x0, y0 = self.inventory(self.center)
        x1, y1 = self.inventory(endpoint)
        net_input = y1 - y0
        gross_input = net_input / (1 - fee)
        fee_paid = gross_input * fee
        curve_shortfall = external * (x0 - x1) - net_input
        arb_profit = curve_shortfall - fee_paid
        lp_fee = fee_paid * lp_fee_share
        return {"jump": jump, "fee_bps": fee_bps, "gross_quote": gross_input,
                "curve_shortfall": curve_shortfall, "fee_paid": fee_paid,
                "lp_fee": lp_fee, "arb_profit": arb_profit,
                "lp_net_shortfall": curve_shortfall - lp_fee, "endpoint": endpoint}


def build_results(capital=1_000_000, annual_volatility=1.0, core_bps=100, fade_bps=400,
                  fee_bps=30, lp_fee_share=0.8):
    scenarios, executions, shocks, feasibility, sensitivity, depth = [], [], [], [], [], []
    for amp in [1, 2, 5, 10]:
        c = Curve(capital, amp, core_bps if amp > 1 else 0, fade_bps if amp > 1 else 0)
        lvr = c.lvr_daily(c.center, annual_volatility)
        scenarios.append({"amplification": amp, "capital_usd": capital,
                          "core_bps": c.core_bps, "fade_bps": c.fade_bps,
                          "tail_depth_vs_v1": c.tail / c.reference_liquidity,
                          "shoulder_depth_vs_v1": (c.tail + c.concentrated / 2) / c.reference_liquidity if amp > 1 else 1,
                          "gross_lvr_usd_day": lvr, "gross_lvr_bps_day": lvr / capital * 10000,
                          "same_peak_depth_capital_usd": capital / amp,
                          "same_peak_depth_capital_saved": 1 - 1 / amp,
                          "incremental_residual_reduction_needed": 1 - 1 / amp,
                          "fee_turnover_to_cover_gross_lvr": lvr / capital / (fee_bps / 10000 * lp_fee_share)})
        for quote in [100, 1000, 10000, 50000, 100000]:
            for fee in [0, fee_bps]:
                executions.append({"amplification": amp, "capital_usd": capital, "fee_bps": fee,
                                   **c.buy(quote, fee)})
        for jump in [0.001, 0.003, 0.01, 0.02, 0.05, 0.10, 0.30]:
            for fee in [fee_bps, 100]:
                shocks.append({"amplification": amp, "capital_usd": capital,
                               "lp_fee_share": lp_fee_share, **c.jump_arbitrage(jump, fee, lp_fee_share)})
        for sigma in [0.5, 1, 1.5, 2]:
            for residual in [1, 0.5, 0.2, 0.1]:
                sensitivity.append({"amplification": amp, "annual_volatility": sigma,
                                    "residual_fraction_assumed": residual,
                                    "residual_cost_bps_day": c.lvr_daily(1, sigma) / capital * 10000 * residual,
                                    "required_organic_turnover": c.lvr_daily(1, sigma) / capital * residual / (fee_bps / 10000 * lp_fee_share)})
        for displacement in [-20, -10, -5, -4, -2, -1, -0.9, -0.5, 0, 0.5, 0.9, 1.1, 2, 4, 5.1, 10, 20]:
            p = 1 + displacement / 100
            depth.append({"amplification": amp, "price_change_pct": displacement,
                          "active_depth_vs_v1_same_initial_capital": c.active_liquidity(p) / c.reference_liquidity,
                          "current_nav_usd": c.nav(p), "capital_usd": capital,
                          "gross_lvr_bps_day_current_nav": c.lvr_daily(p, annual_volatility) / c.nav(p) * 10000})
    for core, fade in [(50, 50), (100, 400), (500, 1500), (1000, 4000)]:
        inv = (1 / math.sqrt(1 + core / 10000) + 1 / math.sqrt(1 + (core + fade) / 10000)) / 2
        bound = 1 / (1 - inv)
        feasibility.append({"core_bps": core, "fade_bps": fade, "strict_amplification_upper_bound": bound,
                            "ten_times_feasible": 10 < bound})
    return {"scenarios": scenarios, "executions": executions, "shocks": shocks,
            "feasibility": feasibility, "sensitivity": sensitivity, "depth": depth}


def validate_model():
    """Independent analytical and accounting checks, including boundaries."""
    checks = []
    for amp in [1, 2, 5, 10]:
        c = Curve(amplification=amp, core_bps=100 if amp > 1 else 0, fade_bps=400 if amp > 1 else 0)
        x, y = c.inventory(1)
        assert math.isclose(x + y, c.capital, rel_tol=1e-12)
        assert math.isclose(x, y, rel_tol=1e-12)
        assert math.isclose(c.lvr_daily(1, 1), amp * c.capital / (8 * 365), rel_tol=1e-12)
        e = 1e-5
        derivative = (c.inventory(1 + e)[0] - c.inventory(1 - e)[0]) / (2 * e)
        assert math.isclose(-derivative, c.active_liquidity(1) / 2, rel_tol=1e-8)
        lower_capital = Curve(c.capital / amp, amp, c.core_bps, c.fade_bps)
        assert math.isclose(lower_capital.lvr_daily(1, 1), Curve().lvr_daily(1, 1), rel_tol=1e-12)
        for p in [0.7, 0.97, 1.005, 1.03, 1.3]:
            xp, yp = c.inventory(p)
            xi, yi = c.inventory(1 / p)
            assert math.isclose(xp, yi, rel_tol=1e-12)
            assert math.isclose(yp, xi, rel_tol=1e-12)
        for _, lo, hi in c.layers:
            for boundary in [lo**2, hi**2]:
                before = c.inventory(boundary * (1 - 1e-10))
                after = c.inventory(boundary * (1 + 1e-10))
                assert all(abs(a - b) < c.capital * 1e-7 for a, b in zip(before, after))
        for jump in [0.001, 0.01, 0.1]:
            result = c.jump_arbitrage(jump)
            assert result['arb_profit'] >= -1e-7
            assert math.isclose(result['lp_net_shortfall'], result['arb_profit'] + result['fee_paid'] - result['lp_fee'], abs_tol=1e-8)
        checks.append(f"A={amp}: centered capital, gamma, equal-depth LVR, reciprocal symmetry, boundary continuity, fee conservation")
    cp = Curve()
    for q in [1, 1000, 100000]:
        assert math.isclose(cp.buy(q)['base_out'], 500000 * q / (500000 + q), rel_tol=1e-9)
    for jump in [0.01, 0.1, 1]:
        expected = 500000 * (math.sqrt(1 + jump) - 1)**2
        assert math.isclose(cp.jump_arbitrage(jump, 0)['arb_profit'], expected, rel_tol=1e-8)
    try:
        Curve(amplification=10, core_bps=1000, fade_bps=4000)
        raise AssertionError("Infeasible curve accepted")
    except ValueError:
        pass
    checks += ["CPMM exact swap formula", "CPMM fee-free jump arbitrage closed form", "Infeasible wide/high-amplification rejection"]
    return checks


def save_results(output_dir):
    output_dir = Path(output_dir)
    results = build_results()
    for name, rows in results.items():
        with (output_dir / f"{name}.csv").open("w", newline="") as handle:
            writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
            writer.writeheader()
            writer.writerows(rows)
    (output_dir / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    checks = validate_model()
    (output_dir / "validation.json").write_text(json.dumps({"status": "passed", "checks": checks}, indent=2) + "\n")
    return results


if __name__ == "__main__":
    data = save_results(Path(__file__).parent)
    print(json.dumps(data['scenarios'], indent=2))
    print(json.dumps(data['feasibility'], indent=2))
