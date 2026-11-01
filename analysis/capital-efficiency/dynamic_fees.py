"""Source-derived dynamic-fee experiments, using real-valued curve arithmetic.

Current gross-path fee ordering, input-denominated fees, no launch premiums,
no compounding, no hLP/debt/cash constraints and a fixed center. This is not
an integer-accurate program replay or an optimized multi-transaction strategy.
"""
from dataclasses import dataclass, replace, asdict
from pathlib import Path
import csv
import json
import math

from model import Curve


@dataclass(frozen=True)
class FeeConfig:
    base_bps: float = 30
    divergence_coefficient: float = 10
    volatility_coefficient: float = 1
    divergence_cap_bps: float = 200
    volatility_cap_bps: float = 200
    volatility_half_life_seconds: float = 60
    shock_cap: float = 0.05
    accumulator_cap: float = 0.10
    protocol_base_share: float = 0.20

    def __post_init__(self):
        assert 0 <= self.base_bps + self.divergence_cap_bps + self.volatility_cap_bps <= 5000
        assert min(self.base_bps, self.divergence_cap_bps, self.volatility_cap_bps) >= 0
        assert 0 <= self.divergence_coefficient <= 100 and 0 <= self.volatility_coefficient <= 100
        assert self.volatility_half_life_seconds > 0
        assert 0 <= self.shock_cap <= self.accumulator_cap <= 10
        assert 0 <= self.protocol_base_share <= 1


def decay(accumulator, elapsed_seconds, config):
    if accumulator < 0 or elapsed_seconds < 0:
        raise ValueError("Negative volatility state or elapsed time")
    return min(accumulator, config.accumulator_cap) * 2 ** (-elapsed_seconds / config.volatility_half_life_seconds)


def potential(outward, center_input, coefficient):
    # Live gross_path_divergence_fee_raw uses this uncapped analytic potential,
    # followed by component/total budgets. It does not use the cfg(test) Huber solver.
    outward = max(0, outward)
    return 4 * coefficient * outward**3 / (3 * center_input * (center_input + outward))


def fee_amounts(gross, start_input, center_input, decayed_accumulator, config):
    if gross < 0 or center_input <= 0:
        raise ValueError("Invalid input")
    base = gross * config.base_bps / 10000
    pressure = config.volatility_coefficient * decayed_accumulator
    vol = min((gross - base) * pressure / (1 + pressure),
              gross * config.volatility_cap_bps / 10000, 0.5 * gross - base)
    provisional = gross - base - vol
    surcharge = potential(start_input + provisional - center_input, center_input, config.divergence_coefficient)
    surcharge -= potential(start_input - center_input, center_input, config.divergence_coefficient)
    divergence = max(0, min(surcharge, gross * config.divergence_cap_bps / 10000, 0.5 * gross - base - vol))
    return {"base_fee": base, "volatility_fee": vol, "distance_fee": divergence,
            "net_input": provisional - divergence, "total_fee": base + vol + divergence}


class Market:
    def __init__(self, curve):
        if curve.center != 1:
            raise ValueError("This experiment normalizes the initial center price to 1")
        self.curve = curve
        self.tail = curve.tail
        self.layers = curve.layers
        self.center_input = curve.capital / 2
        bounds = sorted({p for _, lo, hi in self.layers for p in (lo, hi)})
        self.regions = []
        lo = 0.0
        for hi in bounds + [math.inf]:
            sample = hi / 2 if lo == 0 else (lo * 2 if math.isinf(hi) else (lo + hi) / 2)
            active, intercept = self.tail, 0.0
            for liq, a, b in self.layers:
                if sample >= b:
                    intercept += liq * (b - a)
                elif sample > a:
                    active += liq
                    intercept -= liq * a
            bound_quote = math.inf if math.isinf(hi) else self.point(hi * hi)[1]
            self.regions.append((bound_quote, active, intercept))
            lo = hi

    def point(self, price):
        s = math.sqrt(price)
        x, y = self.tail / s, self.tail * s
        for liq, a, b in self.layers:
            clipped = min(b, max(a, s))
            x += liq * (1 / clipped - 1 / b)
            y += liq * (clipped - a)
        return x, y

    def price_at_quote(self, target):
        for bound, active, intercept in self.regions:
            if target <= bound:
                return ((target - intercept) / active) ** 2
        raise AssertionError("Positive tail must cover every quote reserve")

    def trade(self, gross, start, external, acc, config, buy=True):
        x0, y0 = self.point(start)
        start_input = y0 if buy else x0
        fees = fee_amounts(gross, start_input, self.center_input, acc, config)
        inverse_price = self.price_at_quote(start_input + fees['net_input'])
        end = inverse_price if buy else 1 / inverse_price
        x1, y1 = self.point(end)
        output = max(0, x0 - x1) if buy else max(0, y0 - y1)
        input_value = 1 if buy else external
        output_value = external if buy else 1
        arb = output * output_value - gross * input_value
        protocol = fees['base_fee'] * config.protocol_base_share * input_value
        dynamic = (fees['distance_fee'] + fees['volatility_fee']) * input_value
        curve_shortfall = output * output_value - fees['net_input'] * input_value
        lp_distributed = (fees['base_fee'] * (1-config.protocol_base_share) + fees['distance_fee'] + fees['volatility_fee']) * input_value
        lp_retained = fees['base_fee'] * (1-config.protocol_base_share) * input_value
        shock = max(start / end, end / start) - 1
        post_acc = min(config.accumulator_cap, acc + min(shock, config.shock_cap))
        return {**fees, "gross_input": gross, "output": output, "start_price": start, "external_price": external,
                "end_price": end, "arb_profit_usd": arb, "protocol_fee_usd": protocol,
                "dynamic_fee_usd": dynamic, "lp_fee_distributed_usd": lp_distributed,
                "lp_fee_retained_usd": lp_retained, "curve_shortfall_usd": curve_shortfall,
                "lp_loss_distributed_usd": curve_shortfall-lp_distributed,
                "lp_loss_retained_usd": curve_shortfall-lp_retained,
                "effective_fee_bps": fees['total_fee']/gross*10000 if gross else 0,
                "post_accumulator": post_acc, "buy": buy}

    def optimal_trade(self, start, external, acc, config, grid_size=100):
        buy = external >= start
        p0, pe = self.point(start), self.point(external)
        net_to_external = abs(pe[1]-p0[1]) if buy else abs(pe[0]-p0[0])
        # Total fee <=50%; an optimum does not need to execute beyond external price.
        upper = 2 * net_to_external
        empty = self.trade(0, start, external, acc, config, buy)
        if upper < 1e-10:
            return empty
        def profit(gross):
            return self.trade(gross, start, external, acc, config, buy)['arb_profit_usd']
        points = sorted({0.0, upper, *(upper*i/grid_size for i in range(1, grid_size)),
                         *(upper*10**(-i/2) for i in range(1, 15))})
        profits = [profit(g) for g in points]
        candidates = [(profits[i], g) for i, g in enumerate(points)]
        golden = (math.sqrt(5)-1)/2
        for i in range(1, len(points)-1):
            if profits[i] >= profits[i-1] and profits[i] >= profits[i+1]:
                left, right = points[i-1], points[i+1]
                a, b = right-golden*(right-left), left+golden*(right-left)
                fa, fb = profit(a), profit(b)
                for _ in range(65):
                    if fa > fb:
                        right, b, fb = b, a, fa
                        a = right-golden*(right-left)
                        fa = profit(a)
                    else:
                        left, a, fa = a, b, fb
                        b = left+golden*(right-left)
                        fb = profit(b)
                g = (left+right)/2
                candidates.append((profit(g), g))
        value, gross = max(candidates)
        return empty if value <= 1e-8 else self.trade(gross, start, external, acc, config, buy)


def burst(market, external, acc, config):
    """Sequential greedy arbitrage at a fixed external price, zero elapsed slots.

    Each trade maximizes immediate profit and updates volatility. This is not
    a globally optimized splitting strategy or a full price-path backtest.
    """
    price, trades = 1.0, []
    for _ in range(40):
        trade = market.optimal_trade(price, external, acc, config)
        if trade['arb_profit_usd'] <= 1e-6:
            return trades
        trades.append(trade)
        price, acc = trade['end_price'], trade['post_accumulator']
    raise AssertionError("Greedy burst failed to reach the modeled fee boundary")


def experiments():
    base = FeeConfig()
    profiles = [
        ("v1 base only", 1, replace(base, divergence_coefficient=0, volatility_coefficient=0), 0),
        ("5x base only", 5, replace(base, divergence_coefficient=0, volatility_coefficient=0), 0),
        ("5x dynamic, quiet", 5, base, 0),
        ("5x dynamic, warm", 5, base, 0.005),
        ("5x dynamic, hot", 5, base, 0.01),
    ]
    rows, execution, cooldown, coefficients, sequences = [], [], [], [], []
    for name, amp, config, acc in profiles:
        curve = Curve(amplification=amp, core_bps=100 if amp > 1 else 0, fade_bps=400 if amp > 1 else 0)
        market = Market(curve)
        for jump in [0.005, 0.01, 0.02, 0.05, 0.10, -0.01]:
            row = market.optimal_trade(1, 1+jump, acc, config)
            rows.append({"profile": name, "amplification": amp, "external_jump": jump,
                         "prior_accumulator": acc, **row})
        trade = market.trade(10000, 1, 1, acc, config)
        target_cost_bps = Curve().buy(10000, 30)['average_cost_bps']
        low_capital, high_capital = 1.0, 10_000_000.0
        for _ in range(80):
            candidate = (low_capital+high_capital)/2
            candidate_curve = Curve(capital=candidate, amplification=amp,
                                    core_bps=100 if amp > 1 else 0, fade_bps=400 if amp > 1 else 0)
            candidate_output = Market(candidate_curve).trade(10000, 1, 1, acc, config)['output']
            cost = (10000/candidate_output-1)*10000
            if cost > target_cost_bps:
                low_capital = candidate
            else:
                high_capital = candidate
        capital_required = (low_capital+high_capital)/2
        execution.append({"profile": name, "amplification": amp, "prior_accumulator": acc,
                          "average_cost_bps": (10000/trade['output']-1)*10000,
                          "capital_for_v1_execution_usd": capital_required,
                          "capital_saved_same_execution": 1-capital_required/1_000_000, **trade})
        for jump in [0.01, 0.05]:
            trades = burst(market, 1+jump, acc, config)
            sequences.append({"profile": name, "external_jump": jump, "trade_count": len(trades),
                              **{key: sum(t[key] for t in trades) for key in ['arb_profit_usd', 'lp_loss_distributed_usd', 'lp_loss_retained_usd', 'dynamic_fee_usd']},
                              "final_price": trades[-1]['end_price'] if trades else 1})
    market = Market(Curve(amplification=5, core_bps=100, fade_bps=400))
    for elapsed in [0, 15, 30, 60, 120, 300, 600]:
        acc = decay(0.005, elapsed, base)
        cooldown.append({"elapsed_seconds": elapsed, "prior_accumulator": acc,
                         **market.optimal_trade(1, 1.01, acc, base)})
    for coefficient in [0, 0.1, 1, 5, 10, 25, 50, 100]:
        config = replace(base, divergence_coefficient=coefficient)
        for acc in [0, 0.005]:
            row = market.optimal_trade(1, 1.01, acc, config)
            coefficients.append({"divergence_coefficient": coefficient, "prior_accumulator": acc, **row})
    return {"dynamic_shocks": rows, "dynamic_execution": execution, "dynamic_cooldown": cooldown,
            "dynamic_coefficients": coefficients, "dynamic_bursts": sequences, "config": asdict(base)}


def validate():
    base = FeeConfig()
    no_dynamic = replace(base, divergence_coefficient=0, volatility_coefficient=0)
    for amp in [1, 2, 5, 10]:
        c = Curve(amplification=amp, core_bps=100 if amp > 1 else 0, fade_bps=400 if amp > 1 else 0)
        market = Market(c)
        for price in [0.5, 1/1.05, 1/1.01, 1, 1.01, 1.05, 2]:
            assert math.isclose(market.price_at_quote(market.point(price)[1]), price, rel_tol=1e-12)
        for jump in [0.01, 0.05]:
            actual = market.optimal_trade(1, 1+jump, 0, no_dynamic)
            expected = c.jump_arbitrage(jump)
            assert math.isclose(actual['arb_profit_usd'], expected['arb_profit'], abs_tol=1e-6)
            assert math.isclose(actual['lp_loss_distributed_usd'], expected['lp_net_shortfall'], abs_tol=1e-4)
        for gross in [100, 10000, 100000]:
            buy = market.trade(gross, 1, 1.01, 0.005, base)
            sell = market.trade(gross, 1, 1/1.01, 0.005, base, False)
            assert math.isclose(buy['end_price'] * sell['end_price'], 1, rel_tol=1e-12)
            assert math.isclose(buy['total_fee'], sell['total_fee'], rel_tol=1e-12)
            assert math.isclose(buy['lp_loss_distributed_usd'], buy['arb_profit_usd']+buy['protocol_fee_usd'], abs_tol=1e-7)
            assert math.isclose(buy['lp_loss_retained_usd']-buy['lp_loss_distributed_usd'], buy['dynamic_fee_usd'], abs_tol=1e-7)
            assert buy['total_fee'] <= gross/2
    # Independent exact-rational fee expression, including units.
    center_raw, u_raw, coeff_nad, nad = 500000*10**6, 10000*10**6, 10*10**9, 10**9
    exact_raw = 4*coeff_nad*u_raw**3 // (3*nad*center_raw*(center_raw+u_raw))
    assert abs(potential(10000, 500000, 10)*10**6-exact_raw) < 1
    assert fee_amounts(100, 400000, 500000, 0, base)['distance_fee'] == 0
    assert fee_amounts(100, 500000, 500000, 0, base)['volatility_fee'] == 0
    assert math.isclose(decay(0.005, 60, base), 0.0025)
    m = Market(Curve(amplification=5, core_bps=100, fade_bps=400))
    for acc in [0, 0.005]:
        a = m.optimal_trade(1, 1.01, acc, base)
        b = m.optimal_trade(1, 1.01, acc, base, grid_size=1000)
        assert math.isclose(a['arb_profit_usd'], b['arb_profit_usd'], abs_tol=1e-6)
    return ["Piecewise inverse matches forward curve at boundaries", "Base-only optimizer matches prior closed-form jump benchmark",
            "Buy/sell reciprocal symmetry", "Protocol/base/LP/retention accounting conservation", "50% total fee bound",
            "Distance potential agrees with independent exact-rational atom expression", "Restorative flow has zero distance fee",
            "Quiet accumulator has zero volatility fee", "Half-life decay", "100-point optimizer matches 1000-point bracketing"]


if __name__ == '__main__':
    output = Path(__file__).parent
    checks = validate()
    data = experiments()
    for name, rows in data.items():
        if isinstance(rows, list):
            with (output / f'{name}.csv').open('w', newline='') as f:
                writer = csv.DictWriter(f, fieldnames=rows[0].keys())
                writer.writeheader()
                writer.writerows(rows)
    (output / 'dynamic_results.json').write_text(json.dumps(data, indent=2)+'\n')
    (output / 'dynamic_validation.json').write_text(json.dumps({'status': 'passed', 'checks': checks}, indent=2)+'\n')
    print(json.dumps([r for r in data['dynamic_shocks'] if r['external_jump'] == 0.01], indent=2))
    print('BURSTS', json.dumps(data['dynamic_bursts'], indent=2))
