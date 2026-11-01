"""Fixed-fee concentration experiments; synthetic research, not a live replay.

Only amplification varies across Dusk configurations. Fees, widths, ownership,
center, capital and external opportunities are held fixed within each experiment.
Path trading P&L is measured against the current external mark at each trade,
excluding directional inventory gains between trades. Fees are distributed and
valued immediately in quote units; neither engine compounds them in this model.
"""
from dataclasses import asdict, replace
from datetime import datetime, timezone
from pathlib import Path
import csv
import hashlib
import json
import math
import random
import subprocess

from model import Curve, validate_model
from dynamic_fees import FeeConfig, Market, burst, decay, validate as validate_dynamic

ROOT = Path(__file__).resolve().parent
CAPITAL = 1_000_000.0
AMPLIFICATIONS = [1.25, 1.5, 2, 3, 4, 5, 7.5, 10, 15, 20, 30, 50]
STATES = {"quiet": 0.0, "cool": 0.0025, "warm": 0.005, "hot": 0.01}
JUMPS = [0.001, 0.005, 0.01, 0.02, 0.05, 0.10, -0.01, -0.05]
FIXED_FEES = FeeConfig()
V1_FEES = replace(FIXED_FEES, divergence_coefficient=0, volatility_coefficient=0)


def make_market(amplification, capital=CAPITAL):
    return Market(Curve(capital, amplification,
                        100 if amplification > 1 else 0,
                        400 if amplification > 1 else 0))


def profile(amplification):
    return "Omnipair v1" if amplification == 1 else f"Dusk {amplification:g}x"


def execution_cost(trade, gross, external=1.0, buy=True):
    output_usd = trade['output'] * (external if buy else 1.0)
    input_usd = gross * (1.0 if buy else external)
    return 10000 * (input_usd / output_usd - 1) if output_usd else math.inf


def matched_capital(amplification, notional, accumulator, target_bps):
    low, high = 100.0, CAPITAL
    for _ in range(25):
        trade = make_market(amplification, high).trade(notional, 1, 1, accumulator, FIXED_FEES)
        if execution_cost(trade, notional) <= target_bps:
            break
        high *= 2
    else:
        # A small order's target may be below the asymptotic volatility toll.
        return None
    for _ in range(65):
        candidate = (low + high) / 2
        trade = make_market(amplification, candidate).trade(notional, 1, 1, accumulator, FIXED_FEES)
        if execution_cost(trade, notional) > target_bps:
            low = candidate
        else:
            high = candidate
    return (low + high) / 2


def event_experiments():
    shocks, execution, matched, bursts, geometry = [], [], [], [], []
    for amp in [1, *AMPLIFICATIONS]:
        fees = V1_FEES if amp == 1 else FIXED_FEES
        market = make_market(amp)
        curve = market.curve
        geometry.append({"profile": profile(amp), "amplification": amp,
                         "capital_usd": CAPITAL, "core_bps": curve.core_bps,
                         "fade_bps": curve.fade_bps,
                         "tail_depth_vs_v1": curve.tail / curve.reference_liquidity})
        for state, acc in STATES.items():
            meta = {"profile": profile(amp), "amplification": amp, "state": state,
                    "prior_accumulator": acc, "capital_usd": CAPITAL}
            for jump in JUMPS:
                trade = market.optimal_trade(1, 1 + jump, acc, fees)
                shocks.append({**meta, "external_jump": jump,
                               "net_loss_bps_capital": trade['lp_loss_distributed_usd'] / CAPITAL * 10000,
                               **trade})
            for jump in [0.01, 0.05, 0.10]:
                trades = burst(market, 1 + jump, acc, fees)
                totals = {k: sum(t[k] for t in trades) for k in [
                    'lp_loss_distributed_usd', 'curve_shortfall_usd',
                    'lp_fee_distributed_usd', 'arb_profit_usd', 'protocol_fee_usd']}
                bursts.append({**meta, "external_jump": jump, "trade_count": len(trades),
                               "final_price": trades[-1]['end_price'] if trades else 1,
                               "net_loss_bps_capital": totals['lp_loss_distributed_usd'] / CAPITAL * 10000,
                               **totals})
            for notional in [1000, 10000, 50000]:
                trade = market.trade(notional, 1, 1, acc, fees)
                target = execution_cost(make_market(1).trade(notional, 1, 1, 0, V1_FEES), notional)
                required = CAPITAL if amp == 1 else matched_capital(amp, notional, acc, target)
                row = {**meta, "notional_usd": notional, "target_v1_cost_bps": target,
                       "average_cost_bps": execution_cost(trade, notional),
                       "matched_capital_usd": required,
                       "capital_saved_fraction": None if required is None else 1-required/CAPITAL,
                       "match_status": "unattainable_fee_floor" if required is None else "matched",
                       "output": trade['output'], "total_fee": trade['total_fee']}
                execution.append(row)
                if required is not None and notional == 10000:
                    resized = make_market(amp, required)
                    for jump in [0.01, 0.05, 0.10]:
                        trades = burst(resized, 1+jump, acc, fees)
                        loss = sum(t['lp_loss_distributed_usd'] for t in trades)
                        matched.append({**row, "capital_usd": required, "external_jump": jump,
                                        "lp_loss_distributed_usd": loss,
                                        "curve_shortfall_usd": sum(t['curve_shortfall_usd'] for t in trades),
                                        "lp_fee_distributed_usd": sum(t['lp_fee_distributed_usd'] for t in trades),
                                        "net_loss_bps_capital": loss/required*10000,
                                        "trade_count": len(trades)})
    return {"concentration_shocks": shocks, "concentration_execution": execution,
            "concentration_matched": matched, "concentration_bursts": bursts,
            "concentration_geometry": geometry}


def path_inputs(seed=73021):
    """Four one-hour deterministic paths, sampled every 30 seconds.

    The same seeded, uninformed order intentions are offered to every pool.
    Orders must meet a 150-bps all-in external-mid execution limit to fill.
    """
    rng = random.Random(seed)
    directions = [rng.choice([True, False]) for _ in range(120)]
    result = {}
    for name in ["flat", "slow_5pct_trend", "quiet_then_5pct_jump", "choppy_1pct"]:
        events = []
        for step in range(1, 121):
            if name == "flat":
                price = 1.0
            elif name == "slow_5pct_trend":
                price = 1.05 ** (step / 120)
            elif name == "quiet_then_5pct_jump":
                price = 1.0 if step < 60 else 1.05
            else:
                price = math.exp(math.log(1.01) * math.sin(2*math.pi*step/12))
            events.append({"step": step, "seconds": 30*step, "external_price": price,
                           "customer_arrives": step % 2 == 0, "buy": directions[step-1]})
        result[name] = events
    return result


def replay(amplification, events, customer_notional, limit_bps=150, grid_size=100):
    market = make_market(amplification)
    fees = V1_FEES if amplification == 1 else FIXED_FEES
    price, acc = 1.0, 0.0
    last_external, inventory_beta = 1.0, 0.0
    totals = {"lp_fees_usd": 0.0, "pre_fee_execution_edge_usd": 0.0,
              "net_trading_usd": 0.0, "arb_net_trading_usd": 0.0,
              "customer_net_trading_usd": 0.0, "customer_volume_usd": 0.0,
              "arb_volume_usd": 0.0, "customer_fills": 0, "customer_rejections": 0,
              "arb_trades": 0, "customer_intents": 0, "max_accumulator": 0.0}

    def execute(trade, kind):
        nonlocal price, acc
        price, acc = trade['end_price'], trade['post_accumulator']
        edge = -trade['curve_shortfall_usd']
        revenue = trade['lp_fee_distributed_usd']
        pnl = edge + revenue
        totals['pre_fee_execution_edge_usd'] += edge
        totals['lp_fees_usd'] += revenue
        totals['net_trading_usd'] += pnl
        totals[f'{kind}_net_trading_usd'] += pnl
        totals[f'{kind}_volume_usd'] += trade['gross_input'] * (1 if trade['buy'] else trade['external_price'])
        totals['max_accumulator'] = max(totals['max_accumulator'], acc)
        totals['customer_fills' if kind == 'customer' else 'arb_trades'] += 1

    def settle(external):
        for _ in range(40):
            trade = market.optimal_trade(price, external, acc, fees, grid_size=grid_size)
            if trade['arb_profit_usd'] <= 1e-6:
                return
            execute(trade, 'arb')
        raise AssertionError('Path greedy arbitrage did not settle')

    for event in events:
        acc = decay(acc, 30, fees)
        external = event['external_price']
        inventory_beta += market.point(price)[0] * (external-last_external)
        last_external = external
        settle(external)
        if customer_notional and event['customer_arrives']:
            totals['customer_intents'] += 1
            gross = customer_notional / (1 if event['buy'] else external)
            trade = market.trade(gross, price, external, acc, fees, event['buy'])
            if execution_cost(trade, gross, external, event['buy']) <= limit_bps:
                execute(trade, 'customer')
                settle(external)
            else:
                totals['customer_rejections'] += 1
    final_base, final_quote = market.point(price)
    final_inventory_nav = final_base*last_external+final_quote
    ledger_trading_pnl = final_inventory_nav-CAPITAL-inventory_beta+totals['lp_fees_usd']
    return {"profile": profile(amplification), "amplification": amplification,
            "capital_usd": CAPITAL, "duration_seconds": 3600,
            "customer_notional_usd": customer_notional, "routing_limit_bps": limit_bps,
            "final_pool_price": price, "final_external_price": events[-1]['external_price'],
            "final_accumulator": acc, "net_trading_bps_capital": totals['net_trading_usd']/CAPITAL*10000,
            "final_inventory_nav_usd": final_inventory_nav,
            "inventory_price_change_pnl_usd": inventory_beta,
            "ledger_net_trading_usd": ledger_trading_pnl,
            **totals}


def experiments():
    data = event_experiments()
    paths = path_inputs()
    data['concentration_paths'] = [
        {"path": name, **replay(amp, events, flow)}
        for amp in [1, *AMPLIFICATIONS] for name, events in paths.items()
        for flow in [0, 2000, 10000]]
    data['path_inputs'] = paths
    data['concentration_demand_sensitivity'] = []
    # The hard routing limit can create discontinuous fills. Test both order
    # sequencing and tolerance before treating a winning concentration as robust.
    for seed, limit in [(73021,100),(73021,300),(73022,150),(73023,150)]:
        for name, events in path_inputs(seed).items():
            for amp in [1,2,3,4,5,10,20,50]:
                for flow in [2000,10000]:
                    data['concentration_demand_sensitivity'].append(
                        {"path":name,"order_seed":seed,**replay(amp,events,flow,limit)})
    data['parameters'] = {"amplifications": AMPLIFICATIONS, "fees": asdict(FIXED_FEES),
                          "capital_usd": CAPITAL, "states": STATES, "jumps": JUMPS,
                          "core_bps": 100, "fade_bps": 400,
                          "path_order_seed": 73021, "path_routing_limit_bps": 150,
                          "lp_fee_destination": "distributed", "fee_compounding": False,
                          "center_policy": "fixed at 1", "adaptive_amplification": False}
    return data


def validate(data):
    checks = validate_model() + validate_dynamic()
    for row in data['concentration_shocks']:
        assert math.isclose(row['lp_loss_distributed_usd'],
                            row['arb_profit_usd']+row['protocol_fee_usd'], abs_tol=1e-6)
        assert row['lp_loss_distributed_usd'] >= -1e-7
    for row in data['concentration_execution']:
        if row['matched_capital_usd'] is None:
            rate = FIXED_FEES.volatility_coefficient * row['prior_accumulator']
            retained = (1-FIXED_FEES.base_bps/10000)/(1+rate)
            assert (1/retained-1)*10000 >= row['target_v1_cost_bps']-1e-6
            continue
        fees = V1_FEES if row['amplification'] == 1 else FIXED_FEES
        trade = make_market(row['amplification'], row['matched_capital_usd']).trade(
            row['notional_usd'], 1, 1, row['prior_accumulator'], fees)
        assert abs(execution_cost(trade, row['notional_usd'])-row['target_v1_cost_bps']) < 1e-5
    for row in data['concentration_paths']+data['concentration_demand_sensitivity']:
        assert math.isclose(row['net_trading_usd'], row['lp_fees_usd']+row['pre_fee_execution_edge_usd'], abs_tol=1e-6)
        assert math.isclose(row['net_trading_usd'], row['customer_net_trading_usd']+row['arb_net_trading_usd'], abs_tol=1e-6)
        assert math.isclose(row['net_trading_usd'],row['ledger_net_trading_usd'],abs_tol=1e-5)
        assert row['customer_intents'] == row['customer_fills']+row['customer_rejections']
        assert row['max_accumulator'] <= FIXED_FEES.accumulator_cap
        if row['path'] == 'flat' and row['customer_notional_usd'] == 0:
            assert row['net_trading_usd'] == 0 and row['arb_trades'] == 0
    for amp in [1.25, 5, 10, 50]:
        for start, external in [(1,1.01),(1,1.05),(1.03,1.0),(0.96,1.0)]:
            a = make_market(amp).optimal_trade(start, external, 0.005, FIXED_FEES)
            b = make_market(amp).optimal_trade(start, external, 0.005, FIXED_FEES, grid_size=1000)
            assert abs(a['arb_profit_usd']-b['arb_profit_usd']) < 1e-5
        for acc in [0, .005]:
            a = make_market(amp).optimal_trade(1,1.05,acc,FIXED_FEES)
            b = make_market(amp, CAPITAL*.613).optimal_trade(1,1.05,acc,FIXED_FEES)
            assert math.isclose(b['lp_loss_distributed_usd'],.613*a['lp_loss_distributed_usd'],abs_tol=1e-4)
    checks += ['All shock rows: LP loss = arbitrage profit + protocol base fee',
               'Every finite capital match repriced; impossible targets checked against fee floor',
               'Every path: fees + execution edge = net trading P&L; customer and arb totals reconcile',
               'Independent endpoint inventory ledger less price-change P&L plus fees reconciles every path',
               'Customer intentions reconcile to filled/rejected orders; flat zero-flow path earns zero',
               'High/low A and both directions: 100 vs 1000-point optimizer brackets agree',
               'Capital-scaled adverse-selection loss matches independently recomputed resized market']
    # Denser optimization on the highest-A path with customer flow catches path accumulation errors.
    original = next(r for r in data['concentration_paths'] if r['amplification']==50 and
                    r['path']=='choppy_1pct' and r['customer_notional_usd']==10000)
    refined = replay(50,data['path_inputs']['choppy_1pct'],10000,grid_size=400)
    assert abs(original['net_trading_usd']-refined['net_trading_usd']) < .01
    assert original['customer_fills'] == refined['customer_fills']
    checks.append('High-A choppy path: denser arbitrage search agrees within one cent and same fills')
    return checks


def write_results(data, checks):
    for name, rows in data.items():
        if isinstance(rows,list):
            with (ROOT/f'{name}.csv').open('w',newline='') as f:
                writer=csv.DictWriter(f,fieldnames=rows[0].keys())
                writer.writeheader()
                writer.writerows(rows)
    (ROOT/'concentration_results.json').write_text(json.dumps(data,indent=2)+'\n')
    repo=ROOT.parent.parent
    files=[ROOT/'model.py',ROOT/'dynamic_fees.py',Path(__file__),
           repo/'programs/dusk/src/transitions/amm/curve.rs',
           repo/'programs/dusk/src/transitions/amm/fees.rs',
           repo/'programs/dusk/src/transitions/amm/mod.rs',
           repo/'programs/dusk/src/transitions/ledger.rs']
    notes={"generated_at":datetime.now(timezone.utc).isoformat(),
           "dusk_head":subprocess.check_output(['git','rev-parse','HEAD'],cwd=repo,text=True).strip(),
           "status":"share with caveats", "checks":checks,
           "sources":[{"path":str(p.relative_to(repo)),"sha256":hashlib.sha256(p.read_bytes()).hexdigest()} for p in files],
           "limitations":["Synthetic paths and uncalibrated demand; no expected market return estimated",
                          "Real-valued input fees; no integer program parity or compute-unit benchmark",
                          "Fixed center and fixed A; no controller, hLP, borrowing, retention or fee compounding",
                          "Greedy arbitrage with discrete 30-second observations, not global strategic splitting",
                          "Three order-direction seeds and three routing limits are sensitivity tests, not statistical confidence intervals or a fitted optimum"]}
    (ROOT/'concentration_validation.json').write_text(json.dumps(notes,indent=2)+'\n')


if __name__=='__main__':
    data=experiments()
    checks=validate(data)
    write_results(data,checks)
    print(json.dumps({"checks":len(checks),"rows":{k:len(v) for k,v in data.items() if isinstance(v,list)}},indent=2))
    for name in data['path_inputs']:
        for flow in [0,2000,10000]:
            rows=[r for r in data['concentration_paths'] if r['path']==name and r['customer_notional_usd']==flow]
            best=max(rows,key=lambda r:r['net_trading_usd'])
            v1=next(r for r in rows if r['amplification']==1)
            print(name,flow,'best',best['profile'],round(best['net_trading_usd'],2),
                  'v1',round(v1['net_trading_usd'],2),'fills',best['customer_fills'])
