"""Causal adaptive-concentration research; historical prices, modeled demand.

Policies cannot see customer labels, rejected intentions, regime schedules, or
future prices. Reconfiguration conserves reserves and exposes any new stale
quote to arbitrage. Governance admission is deliberately relaxed for economic
research, explicitly distinct from current-program execution.
"""
from collections import defaultdict, deque
from concurrent.futures import ProcessPoolExecutor
from datetime import datetime, timezone
from pathlib import Path
import argparse
import csv
import hashlib
import json
import math
import random
import statistics
import time

from concentration_sweep import make_market, FIXED_FEES, V1_FEES, execution_cost
from fee_optimizer import optimal_trade
import shortlist_replay as previous

ROOT = Path(__file__).resolve().parent
CAPITAL = 1_000_000.0
AMPLIFICATIONS = [3, 5, 10]
REGIMES = [(1_000_000, 100), (100_000, 20), (1_000_000, 50), (100_000, 100)]
MODES = ['strong', 'weak', 'cheap', 'changing']
POLICIES = [
    *[{'id': f'fixed_{a}', 'family': 'fixed', 'amplification': a} for a in [1, 3, 5, 10]],
    {'id': 'coverage_150', 'family': 'coverage', 'low': 1.05, 'high': 1.5},
    {'id': 'coverage_200', 'family': 'coverage', 'low': 1.1, 'high': 2},
    {'id': 'coverage_300', 'family': 'coverage', 'low': 1.25, 'high': 3},
    {'id': 'coverage_cost_gate', 'family': 'coverage', 'low': 1.1, 'high': 2, 'cost_gate': True},
    {'id': 'flow_050', 'family': 'flow', 'low': .125, 'high': .5},
    {'id': 'flow_150', 'family': 'flow', 'low': .375, 'high': 1.5},
    {'id': 'volatility_100', 'family': 'volatility', 'low': .000025, 'high': .0001},
    {'id': 'volatility_200', 'family': 'volatility', 'low': .0001, 'high': .0004},
]
_BLOCKS = []
_SIZES = []
_MEAN_SIZE = 0.0


def initialize(sizes, blocks):
    global _SIZES, _BLOCKS, _MEAN_SIZE
    _SIZES, _BLOCKS = sizes, blocks
    _MEAN_SIZE = statistics.fmean(sizes)


def data_inputs():
    manifest = json.loads((ROOT/'market-data/manifest.json').read_text())
    for source in manifest['sources']:
        raw = (ROOT/'market-data'/source['file']).read_bytes()
        assert hashlib.sha256(raw).hexdigest() == source['sha256']
        assert source['sha256'] == (ROOT/'market-data'/(source['file']+'.CHECKSUM')).read_text().split()[0]
    sizes = [float(r[1])*float(r[2]) for r in previous.read_zip('SOLUSDT-aggTrades-2025-01-01.zip')]
    assert len(sizes) == 238579 and all(math.isfinite(s) and s > 0 for s in sizes)
    blocks = []
    for day in ['2025-01-08', '2025-01-09', '2025-01-15', '2025-01-19']:
        rows = previous.read_klines(f'SOLUSDT-1s-{day}.zip', 1_000_000)
        assert len(rows) == 86400
        for offset in range(0, 86400, 21600):
            sub = rows[offset:offset+21600]
            opening = sub[0]['open']
            blocks.append({'index': len(blocks), 'day': day,
                'start_utc': previous.utc(sub[0]['t']), 'hour': offset//3600,
                'split': 'train' if day <= '2025-01-09' else 'evaluation',
                'initial_sol_usdt': opening, 'prices': [r['close']/opening for r in sub]})
    return sizes, blocks


def intentions(block_index, seed, mode, duration=21600):
    rng = random.Random(7000000+100000*block_index+seed)
    regime_rng = random.Random(9000000+100000*block_index+seed)
    orders = defaultdict(list)
    regimes = []
    for segment, start in enumerate(range(0, duration, 1800)):
        regime_index = MODES.index(mode) if mode != 'changing' else regime_rng.randrange(4)
        target, median = REGIMES[regime_index]
        regimes.append({'start_second': start, 'target_per_hour': target, 'median_budget_bps': median})
        elapsed = float(start)
        while True:
            elapsed += rng.expovariate(target/_MEAN_SIZE/3600)
            if elapsed > min(duration, start+1800):
                break
            orders[max(1, math.ceil(elapsed))].append((
                _SIZES[rng.randrange(len(_SIZES))], rng.random() < .5,
                rng.lognormvariate(math.log(median), .5)))
    return orders, regimes


def reshape(market, price, amplification):
    """Find a new homothetic curve through unchanged positive reserves.

    Reserve quote/base ratio is strictly increasing in price. Solving that
    ratio followed by one scale gives the unique positive curve through the
    point, the real-valued analogue of prepare_concentrated_cache_at_point.
    """
    x, y = market.point(price)
    template = make_market(amplification)
    target = y/x
    low, high = -30.0, 30.0
    for _ in range(75):
        log_price = (low+high)/2
        tx, ty = template.point(math.exp(log_price))
        if ty/tx < target:
            low = log_price
        else:
            high = log_price
    new_price = math.exp((low+high)/2)
    tx, _ = template.point(new_price)
    new_market = make_market(amplification, template.curve.capital*x/tx)
    nx, ny = new_market.point(new_price)
    assert abs(nx-x) < 2e-7 and abs(ny-y) < 2e-7
    return new_market, new_price


def estimate_switch_loss(market, price, external, acc, fees):
    cost = 0.0
    for _ in range(40):
        trade = optimal_trade(market, price, external, acc, fees)
        if trade['arb_profit_usd'] <= 1e-6:
            return cost
        cost += trade['lp_loss_distributed_usd']
        price, acc = trade['end_price'], trade['post_accumulator']
    raise AssertionError('Reconfiguration arbitrage estimate did not settle')


class Signals:
    """Bounded past-observation buffers; no classification by trader identity."""
    def __init__(self):
        self.pending = deque()
        self.mature = deque()
        self.volumes = deque()
        self.returns = deque()
        self.credit = self.loss = self.volume = self.rv = 0.0

    def trade(self, second, dx, dy, fee, volume):
        self.pending.append((second+30, dx, dy, fee))
        self.volumes.append((second, volume))
        self.volume += volume

    def tick(self, second, external, external_before):
        square = math.log(external/external_before)**2
        self.returns.append((second, square))
        self.rv += square
        while self.pending and self.pending[0][0] <= second:
            maturity, dx, dy, fee = self.pending.popleft()
            edge = dx*external+dy
            credit, loss = fee+max(0, edge), max(0, -edge)
            self.mature.append((maturity, credit, loss))
            self.credit += credit
            self.loss += loss
        while self.mature and self.mature[0][0] <= second-300:
            _, credit, loss = self.mature.popleft()
            self.credit -= credit
            self.loss -= loss
        while self.volumes and self.volumes[0][0] <= second-300:
            _, volume = self.volumes.popleft()
            self.volume -= volume
        while self.returns and self.returns[0][0] <= second-300:
            _, square = self.returns.popleft()
            self.rv -= square

    def value(self, family):
        if family == 'coverage':
            return max(0, self.credit)/max(1.0, self.loss)
        if family == 'flow':
            return max(0, self.volume)/CAPITAL*12
        if family == 'volatility':
            return max(0, self.rv)*12
        raise ValueError(family)


def simulate(case, *, supplied_orders=None, trace=False, dense=False):
    block = _BLOCKS[case['block_index']]
    policy = next(p for p in POLICIES if p['id'] == case['policy'])
    prices = block['prices'][:case.get('duration', len(block['prices']))]
    amp = policy.get('amplification', 5)
    market = make_market(amp)
    fees = V1_FEES if amp == 1 else FIXED_FEES
    if supplied_orders is None:
        orders, regimes = intentions(block['index'], case['seed'], case['mode'], len(prices))
    else:
        orders, regimes = supplied_orders, []
    price, acc, previous_external = 1.0, 0.0, 1.0
    x, y = market.point(price)
    inventory_beta = total_edge = total_fees = arb_loss = customer_net = 0.0
    base_fees = volatility_fees = outward_fees = 0.0
    offered_volume = filled_volume = arb_volume = 0.0
    intents = fills = arbs = switches = cost_rejections = depth_decreases = cap_exceedances = 0
    switch_loss = switch_price_jump_sum_bps = 0.0
    max_required_depth_fraction = 0.0
    last_switch = -100000
    signals = Signals()
    decisions, checkpoints = [], []
    seconds_by_amp = defaultdict(int)
    peak_pnl = max_drawdown = 0.0
    minimum_pnl = 0.0
    decay_factor = 2**(-1/fees.volatility_half_life_seconds)
    solve = (lambda m, p, e, a, f: m.optimal_trade(p, e, a, f, grid_size=1600)) if dense else optimal_trade

    def execute(trade, kind, second):
        nonlocal price, acc, x, y, total_edge, total_fees, arb_loss, customer_net
        nonlocal base_fees, volatility_fees, outward_fees, filled_volume, arb_volume, fills, arbs
        nonlocal peak_pnl, max_drawdown, minimum_pnl
        old_x, old_y = x, y
        price, acc = trade['end_price'], trade['post_accumulator']
        x, y = market.point(price)
        edge, fee = -trade['curve_shortfall_usd'], trade['lp_fee_distributed_usd']
        total_edge += edge
        total_fees += fee
        mark = 1 if trade['buy'] else trade['external_price']
        volume = trade['gross_input']*mark
        base_fees += trade['base_fee']*mark*(1-fees.protocol_base_share)
        volatility_fees += trade['volatility_fee']*mark
        outward_fees += trade['distance_fee']*mark
        if kind == 'customer':
            customer_net += edge+fee
            filled_volume += volume
            fills += 1
        else:
            arb_loss -= edge+fee
            arb_volume += volume
            arbs += 1
        # Delayed markouts value only the trade's reserve changes at maturity;
        # this diagnostic is never added to accounting P&L a second time.
        signals.trade(second, x-old_x, y-old_y, fee, volume)
        pnl = total_fees+total_edge
        peak_pnl = max(peak_pnl, pnl)
        max_drawdown = max(max_drawdown, peak_pnl-pnl)
        minimum_pnl = min(minimum_pnl, pnl)

    def settle(external, second):
        for _ in range(40):
            if previous.no_profitable_arb(price, external, acc, fees):
                return
            trade = solve(market, price, external, acc, fees)
            if trade['arb_profit_usd'] <= 1e-6:
                return
            execute(trade, 'arbitrage', second)
        raise AssertionError('Arbitrage did not settle')

    for second, external in enumerate(prices, 1):
        # Decisions precede the next reference tick and its arriving orders.
        if policy['family'] != 'fixed' and second > 600 and (second-1)%60 == 0:
            value = signals.value(policy['family'])
            desired = 3 if value < policy['low'] else 10 if value > policy['high'] else 5
            index, target = AMPLIFICATIONS.index(amp), AMPLIFICATIONS.index(desired)
            candidate = AMPLIFICATIONS[index + (1 if target > index else -1 if target < index else 0)]
            decision = {'second': second-1, 'old_amplification': amp, 'desired': desired,
                        'signal': value, 'candidate': candidate, 'executed': False,
                        'observed_through_second': second-1}
            if candidate != amp and second-1-last_switch >= 300:
                next_market, next_price = reshape(market, price, candidate)
                estimated_cost = estimate_switch_loss(next_market, next_price, previous_external, acc, fees)
                limit = .25*max(0, signals.credit-signals.loss)
                decision.update(estimated_cost=estimated_cost, gate_limit=limit)
                if policy.get('cost_gate') and estimated_cost > limit+1e-6:
                    cost_rejections += 1
                else:
                    old_depth = market.curve.capital*amp/2
                    new_depth = next_market.curve.capital*candidate/2
                    if new_depth < old_depth-1e-6:
                        depth_decreases += 1
                        # Source has 125% coverage plus a 1-bps depth guard;
                        # actual authorization additionally has a hard cap.
                        fraction = 1.25*(old_depth-new_depth)/old_depth+.0001
                        cap_exceedances += fraction > .01
                        max_required_depth_fraction = max(max_required_depth_fraction, fraction)
                    switch_price_jump_sum_bps += abs(math.log(next_price/price))*10000
                    before_nav = x*previous_external+y
                    market, price, amp = next_market, next_price, candidate
                    x, y = market.point(price)
                    assert abs(x*previous_external+y-before_nav) < 5e-7
                    before_pnl = total_fees+total_edge
                    settle(previous_external, second-1)
                    actual_cost = before_pnl-total_fees-total_edge
                    assert abs(actual_cost-estimated_cost) < 2e-5
                    switch_loss += actual_cost
                    switches += 1
                    last_switch = second-1
                    decision.update(executed=True, actual_switch_loss=actual_cost)
            if trace:
                decisions.append(decision)
            # Switch trades occur at the last reference; mature existing
            # markouts at the next tick below, without injecting future data.
        acc *= decay_factor
        inventory_beta += x*(external-previous_external)
        old_external = previous_external
        previous_external = external
        settle(external, second)
        for notional, buy, budget in orders.get(second, []):
            offered_volume += notional
            intents += 1
            gross = notional/(1 if buy else external)
            trade = market.trade(gross, price, external, acc, fees, buy)
            if execution_cost(trade, gross, external, buy) <= budget:
                execute(trade, 'customer', second)
                settle(external, second)
        signals.tick(second, external, old_external)
        seconds_by_amp[amp] += 1
        if trace and second%300 == 0:
            checkpoints.append({'second': second, 'amplification': amp,
                'net_trading_quote': total_fees+total_edge, 'switches': switches,
                'switch_loss_quote': switch_loss, 'fees_quote': total_fees,
                'arb_loss_quote': arb_loss, 'filled_volume_quote': filled_volume})
    net = total_edge+total_fees
    ledger = x*previous_external+y-CAPITAL-inventory_beta+total_fees
    assert abs(net-ledger) < .001, (net, ledger)
    assert abs(net-(customer_net-arb_loss)) < 1e-5
    assert abs(total_fees-base_fees-volatility_fees-outward_fees) < 1e-5
    result = {**case, 'day': block['day'], 'hour': block['hour'], 'split': block['split'],
        'family': policy['family'], 'duration_seconds': len(prices), 'net_trading_quote': net,
        'net_trading_bps': net/CAPITAL*10000, 'lp_fees_quote': total_fees,
        'pre_fee_execution_edge_quote': total_edge, 'arb_loss_after_fees_quote': arb_loss,
        'customer_net_quote': customer_net, 'base_lp_fees_quote': base_fees,
        'volatility_fees_quote': volatility_fees, 'outward_fees_quote': outward_fees,
        'offered_volume_quote': offered_volume, 'filled_volume_quote': filled_volume,
        'arb_volume_quote': arb_volume, 'intents': intents, 'fills': fills, 'rejections': intents-fills,
        'arb_trades': arbs, 'switches': switches, 'switch_loss_quote': switch_loss,
        'cost_gate_rejections': cost_rejections, 'depth_reducing_switches': depth_decreases,
        'switches_exceeding_depth_hard_cap': cap_exceedances,
        'max_required_depth_fraction': max_required_depth_fraction,
        'mean_switch_price_jump_bps': switch_price_jump_sum_bps/switches if switches else 0,
        'seconds_at_1': seconds_by_amp[1], 'seconds_at_3': seconds_by_amp[3],
        'seconds_at_5': seconds_by_amp[5], 'seconds_at_10': seconds_by_amp[10],
        'max_trading_drawdown_quote': max_drawdown, 'minimum_trading_pnl_quote': minimum_pnl,
        'endpoint_ledger_quote': ledger, 'inventory_beta_quote': inventory_beta,
        'final_price': price, 'final_accumulator': acc}
    if trace:
        result.update(decisions=decisions, checkpoints=checkpoints, regimes=regimes)
    return result


def save_csv(name, rows):
    with (ROOT/name).open('w', newline='') as handle:
        writer = csv.DictWriter(handle, fieldnames=rows[0].keys())
        writer.writeheader()
        writer.writerows(rows)


def aggregate(rows):
    groups = defaultdict(list)
    for r in rows:
        groups[(r['split'], r['policy'], r['mode'])].append(r)
        groups[(r['split'], r['policy'], 'all')].append(r)
    output = []
    for (split, policy, mode), values in groups.items():
        result = {'split': split, 'policy': policy, 'mode': mode, 'cases': len(values)}
        for field in ['net_trading_quote', 'lp_fees_quote', 'pre_fee_execution_edge_quote',
                      'switches', 'switch_loss_quote', 'depth_reducing_switches', 'filled_volume_quote',
                      'arb_loss_after_fees_quote', 'max_trading_drawdown_quote']:
            result['mean_'+field] = statistics.fmean(v[field] for v in values)
        result['min_net_trading_quote'] = min(v['net_trading_quote'] for v in values)
        result['negative_cases'] = sum(v['net_trading_quote'] < 0 for v in values)
        output.append(result)
    return output


def validate_basic():
    checks = []
    for a in [3, 5, 10]:
        for price in [.8, .96, .998, 1, 1.003, 1.04, 1.2]:
            market = make_market(a)
            for b in [3, 5, 10]:
                new, new_price = reshape(market, price, b)
                assert math.isclose(market.point(price)[0], new.point(new_price)[0], abs_tol=1e-7)
                old, old_price = reshape(new, new_price, a)
                assert abs(old_price-price) < 1e-12
                assert abs(old.curve.capital-market.curve.capital) < 1e-6
    checks.append('63 reserve-preserving reconfigurations and round-trip reconstructions conserve holdings and curve scale')
    # Independent old fixed replay, with identical externally supplied orders.
    original_windows, original_sizes = previous._WINDOWS, previous._SIZES
    block = _BLOCKS[0]
    previous.init_worker(_SIZES, [{'window': 'parity', 'start_utc': block['start_utc'], 'prices': block['prices'][:3600]}])
    orders = previous.intentions(0, 201, 1000000, 100)
    for a in [1, 3, 5, 10]:
        old = previous.run_case({'window_index': 0, 'amplification': a, 'capital_quote': CAPITAL,
            'seed': 201, 'demand_target_quote': 1000000, 'tolerance_median_bps': 100, 'arb_interval_seconds': 1})
        new = simulate({'block_index': 0, 'policy': f'fixed_{a}', 'seed': 201, 'mode': 'strong', 'duration': 3600}, supplied_orders=orders)
        assert abs(old['net_trading_quote']-new['net_trading_quote']) < 1e-7
        assert old['fills'] == new['fills'] and old['arb_trades'] == new['arb_trades']
    previous.init_worker(original_sizes, original_windows)
    checks.append('All four fixed policies reproduce prior engine P&L, fills and arbitrage counts on identical intentions')
    case = {'block_index': 0, 'policy': 'coverage_200', 'seed': 201, 'mode': 'changing', 'duration': 2400}
    full = simulate(case, trace=True)
    prefix = simulate({**case, 'duration': 1200}, trace=True)
    assert prefix['decisions'] == [d for d in full['decisions'] if d['second'] < 1200]
    assert prefix['checkpoints'] == [d for d in full['checkpoints'] if d['second'] <= 1200]
    checks.append('Identical first 20-minute decisions and accounting when all later prices/orders are removed')
    for a in [3, 5, 10]:
        new, p = reshape(make_market(5), 1.015, a)
        solved = optimal_trade(new, p, 1.021, .002, FIXED_FEES)
        dense = new.optimal_trade(p, 1.021, .002, FIXED_FEES, grid_size=1600)
        assert abs(solved['arb_profit_usd']-dense['arb_profit_usd']) < 1e-5
    checks.append('Post-reconfiguration arbitrage solver agrees with independent 1600-point search')
    return checks


def jobs(split, policies, seeds):
    return [{'block_index': b['index'], 'policy': p, 'seed': seed, 'mode': mode}
            for b in _BLOCKS if b['split'] == split for mode in MODES for seed in seeds for p in policies]


def run_batch(cases, workers):
    rows = []
    with ProcessPoolExecutor(max_workers=workers, initializer=initialize, initargs=(_SIZES, _BLOCKS)) as pool:
        for i, row in enumerate(pool.map(simulate, cases), 1):
            rows.append(row)
            if i%100 == 0 or i == len(cases):
                print(f'{row["split"]}: {i}/{len(cases)}', flush=True)
    return rows


def main(workers=4):
    start = time.monotonic()
    sizes, blocks = data_inputs()
    initialize(sizes, blocks)
    checks = validate_basic()
    print('Input and model validation passed', flush=True)
    training = run_batch(jobs('train', [p['id'] for p in POLICIES], [201, 202]), workers)
    summary = aggregate(training)
    selected = {}
    for family in ['fixed', 'coverage', 'flow', 'volatility']:
        eligible = {p['id'] for p in POLICIES if p['family'] == family}
        selected[family] = max((r for r in summary if r['mode'] == 'all' and r['policy'] in eligible),
                               key=lambda r: (r['mean_net_trading_quote'], r['policy']))['policy']
    selection = {'selected': selected, 'training_summary': summary,
                 'frozen_at': datetime.now(timezone.utc).isoformat(),
                 'plan_sha256': hashlib.sha256((ROOT/'adaptive_plan.json').read_bytes()).hexdigest()}
    (ROOT/'adaptive_selection.json').write_text(json.dumps(selection, indent=2)+'\n')
    save_csv('adaptive_training.csv', training)
    print('Frozen before evaluation:', selected, flush=True)
    selected_policies = [f'fixed_{a}' for a in [1, 3, 5, 10]]+[selected[f] for f in ['coverage', 'flow', 'volatility']]
    evaluation = run_batch(jobs('evaluation', selected_policies, [301, 302, 303]), workers)
    rows = training+evaluation
    common = {}
    for r in rows:
        key = (r['block_index'], r['mode'], r['seed'])
        value = (r['offered_volume_quote'], r['intents'])
        assert key not in common or common[key] == value
        common[key] = value
        assert r['switch_loss_quote'] <= r['arb_loss_after_fees_quote']+1e-5
        assert sum(r[f'seconds_at_{a}'] for a in [1, 3, 5, 10]) == r['duration_seconds']
    checks.extend(['Every replay reconciles endpoint reserve NAV minus inventory beta plus fees to trading P&L',
        'All paired policies receive exactly identical intentions, budgets and offered volume',
        'Every executed change preserves both reserves and NAV, and predicted immediate arbitrage cost reconciles to execution',
        'Policy selection file frozen before later-date evaluation begins; evaluation never feeds back into selection'])
    summary = aggregate(rows)
    save_csv('adaptive_replays.csv', rows)
    save_csv('adaptive_summary.csv', summary)
    traced = simulate({'block_index': 15, 'policy': selected['coverage'], 'seed': 301, 'mode': 'changing'}, trace=True)
    (ROOT/'adaptive_trace.json').write_text(json.dumps(traced, indent=2)+'\n')
    outputs = {'parameters': json.loads((ROOT/'adaptive_plan.json').read_text()), 'selected': selected,
        'blocks': [{k:v for k,v in b.items() if k != 'prices'} for b in blocks],
        'summary': summary, 'rows': rows}
    (ROOT/'adaptive_results.json').write_text(json.dumps(outputs, indent=2)+'\n')
    validation = {'status': 'share with caveats', 'replays': len(rows), 'training_replays': len(training),
        'evaluation_replays': len(evaluation), 'evaluation_cases_per_policy': len(evaluation)//7,
        'checks': checks, 'elapsed_seconds': time.monotonic()-start,
        'source_hashes': {n:hashlib.sha256((ROOT/n).read_bytes()).hexdigest() for n in
                          ['adaptive_replay.py', 'adaptive_plan.json', 'fee_optimizer.py', 'dynamic_fees.py', 'model.py']},
        'limitations': ['Historical external prices with synthetic flow; all-swap external markouts require an off-chain reference',
            'Four days in one month; only two later-date evaluation days and three demand seeds',
            'Some evaluation hours were previously explored; no fully blind performance claim',
            'Governance timelock and protected-depth admission deliberately relaxed; current program cannot execute this fast controller',
            'Greedy arbitrage, no strategic manipulation or sandwich/front-running of predictable changes; zero base transaction overhead',
            '30-second markouts are controller signals only; not an additional loss deduction or true toxicity classification',
            'No integer program parity, Solana compute-unit benchmark, active hLP, lending, retention, compounding or recentering']}
    (ROOT/'adaptive_validation.json').write_text(json.dumps(validation, indent=2)+'\n')
    print(json.dumps({'selected': selected, 'evaluation': [r for r in summary if r['split'] == 'evaluation' and r['mode'] == 'all'],
                      'seconds': validation['elapsed_seconds']}, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--workers', type=int, default=4)
    main(parser.parse_args().workers)
