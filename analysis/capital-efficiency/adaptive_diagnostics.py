"""Post-selection diagnosis of the frozen controller evaluation.

The cost-gated candidate was declared before training, but its later-date
evaluation was added after seeing failure of the selected coverage controller.
Treat it as exploratory robustness, not an untouched validation set.
"""
from pathlib import Path
import hashlib
import json
import statistics
import time

import adaptive_replay as a

ROOT = Path(__file__).resolve().parent


def main():
    sizes, blocks = a.data_inputs()
    a.initialize(sizes, blocks)
    cases = a.jobs('evaluation', ['coverage_cost_gate'], [301, 302, 303])
    rows = a.run_batch(cases, 4)
    a.save_csv('adaptive_gated_replays.csv', rows)
    summary = a.aggregate(rows)
    a.save_csv('adaptive_gated_summary.csv', summary)
    (ROOT/'adaptive_gated_results.json').write_text(json.dumps({
        'status': 'exploratory post-selection robustness', 'rows': rows, 'summary': summary,
        'note': __doc__.strip()}, indent=2)+'\n')
    case = {'block_index': 15, 'policy': 'coverage_150', 'seed': 301, 'mode': 'changing', 'duration': 3600}
    normal = a.simulate(case)
    dense = a.simulate(case, dense=True)
    assert normal['switches'] > 0
    assert abs(normal['net_trading_quote']-dense['net_trading_quote']) < .05
    assert normal['fills'] == dense['fills'] and normal['switches'] == dense['switches']
    timings = []
    for _ in range(100):
        market = a.make_market(5)
        start = time.perf_counter()
        a.reshape(market, 1.02, 10)
        timings.append((time.perf_counter()-start)*1000)
    examples = []
    for price in [1, 1.005, 1.02, 1.05]:
        for old, new in [(3, 10), (10, 3), (5, 10), (10, 5)]:
            market = a.make_market(old)
            nxt, p = a.reshape(market, price, new)
            depth, new_depth = market.curve.capital*old/2, nxt.curve.capital*new/2
            cover = 1.25*(depth-new_depth)/depth+.0001 if new_depth < depth-1e-7 else 0
            examples.append({'old_price': price, 'old_amplification': old, 'new_amplification': new,
                'new_price': p, 'switch_loss_quote': a.estimate_switch_loss(nxt, p, price, 0, a.FIXED_FEES),
                'required_depth_fraction': cover, 'exceeds_one_percent_hard_cap': cover > .01})
    a.save_csv('adaptive_switch_examples.csv', examples)
    repo = ROOT.parent.parent
    files = ['programs/dusk/src/transitions/amm/mod.rs', 'programs/dusk/src/transitions/amm/curve.rs',
             'programs/dusk/src/state/market.rs', 'programs/dusk/src/state/parameter_proposal.rs',
             'programs/dusk/src/constants.rs']
    text = (repo/'programs/dusk/src/state/market.rs').read_text()
    assert 'PROTECTED_LIQUIDITY_COVERAGE_BPS: u16 = 12_500' in text
    assert 'PROTECTED_LIQUIDITY_GUARD_BPS: u16 = 1;' in text
    assert 'PROTECTED_LIQUIDITY_CAP_BPS: u16 = 100;' in text
    assert 'PARAMETER_PROPOSAL_TIMELOCK_SECONDS: i64 = 7 * 24 * 60 * 60' in (repo/files[-1]).read_text()
    result = {'dense_adaptive_hour': {'policy': case['policy'], 'net_trading_quote': normal['net_trading_quote'],
        'dense_net_trading_quote': dense['net_trading_quote'], 'fills': normal['fills'], 'switches': normal['switches']},
        'python_reshape_median_ms': statistics.median(timings),
        'benchmark_note': '100 warm Python calls; indicative only, not Solana compute units.',
        'source_hashes': {f: hashlib.sha256((repo/f).read_bytes()).hexdigest() for f in files},
        'method_hashes': {f: hashlib.sha256((ROOT/f).read_bytes()).hexdigest() for f in
                          ['adaptive_replay.py', 'adaptive_diagnostics.py']},
        'governance': {'timelock_seconds': 604800, 'coverage_bps': 12500, 'guard_bps': 1, 'hard_cap_bps': 100},
        'examples': examples}
    (ROOT/'adaptive_extra_validation.json').write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps([r for r in summary if r['mode'] == 'all'], indent=2))


if __name__ == '__main__':
    main()
