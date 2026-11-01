"""Preserve the full report and add adaptive concentration evidence."""
from collections import defaultdict
from copy import deepcopy
from datetime import datetime, timezone
from pathlib import Path
import contextlib
import hashlib
import io
import json
import sqlite3
import statistics

ROOT = Path(__file__).resolve().parent
LABELS = {'fixed_1': 'Omnipair v1', 'fixed_3': 'Fixed 3×', 'fixed_5': 'Fixed 5×',
    'fixed_10': 'Fixed 10×', 'coverage_150': 'Profitability signal',
    'flow_050': 'Volume signal', 'volatility_100': 'Volatility signal',
    'coverage_cost_gate': 'Cost-gated (exploratory)'}
MODES = {'strong': 'Strong demand', 'weak': 'Weak demand', 'cheap': 'Cheap execution',
         'changing': 'Changing demand', 'all': 'All scenarios'}


def build():
    data = json.loads((ROOT/'adaptive_results.json').read_text())
    gated = json.loads((ROOT/'adaptive_gated_results.json').read_text())
    extra = json.loads((ROOT/'adaptive_extra_validation.json').read_text())
    rows = data['rows']+gated['rows']
    expected = data['summary']+gated['summary']
    groups = defaultdict(list)
    for r in rows:
        for mode in [r['mode'], 'all']:
            groups[(r['split'], r['policy'], mode)].append(r)
    columns = ['split', 'policy', 'mode', 'cases', 'sum_fees', 'sum_edge', 'sum_switches',
               'sum_switch_loss', 'sum_drawdown', 'sum_filled', 'sum_cap_rejections', 'negative_cases', 'worst_pnl']
    totals = []
    for key, values in groups.items():
        totals.append((*key, len(values), sum(r['lp_fees_quote'] for r in values),
            sum(r['pre_fee_execution_edge_quote'] for r in values), sum(r['switches'] for r in values),
            sum(r['switch_loss_quote'] for r in values), sum(r['max_trading_drawdown_quote'] for r in values),
            sum(r['filled_volume_quote'] for r in values), sum(r['switches_exceeding_depth_hard_cap'] for r in values),
            sum(r['net_trading_quote'] < 0 for r in values), min(r['net_trading_quote'] for r in values)))
    def literal(v):
        return "'"+v.replace("'", "''")+"'" if isinstance(v, str) else repr(v)
    query = ('WITH totals('+','.join(columns)+') AS (VALUES\n'+
        ',\n'.join('('+','.join(literal(v) for v in r)+')' for r in totals)+')\n'+
        'SELECT *, (sum_fees+sum_edge)/cases AS mean_net_trading, sum_fees/cases AS mean_fees, '+
        '1.0*sum_switches/cases AS mean_switches, sum_switch_loss/cases AS mean_switch_loss, '+
        'sum_drawdown/cases AS mean_drawdown FROM totals;\n')
    (ROOT/'adaptive_verification.sql').write_text(query)
    db = sqlite3.connect(':memory:')
    db.row_factory = sqlite3.Row
    verified = [dict(r) for r in db.execute(query)]
    db.close()
    for r in verified:
        e = next(x for x in expected if all(x[k] == r[k] for k in ['split', 'policy', 'mode']))
        assert abs(r['mean_net_trading']-e['mean_net_trading_quote']) < 1e-6
        assert abs(r['mean_switch_loss']-e['mean_switch_loss_quote']) < 1e-6
        r['label'] = LABELS.get(r['policy'], r['policy'].replace('_', ' '))
        r['demand'] = MODES[r['mode']]
    def get(policy, mode='all', split='evaluation'):
        return next(r for r in verified if r['policy'] == policy and r['mode'] == mode and r['split'] == split)
    fixed, profit, flow, vol, guard = [get(p) for p in
        ['fixed_10', 'coverage_150', 'flow_050', 'volatility_100', 'coverage_cost_gate']]
    gap_pct = 100*(1-profit['mean_net_trading']/fixed['mean_net_trading'])
    guard_gap_pct = 100*(1-guard['mean_net_trading']/fixed['mean_net_trading'])
    drawdown_reduction = 100*(1-guard['mean_drawdown']/fixed['mean_drawdown'])
    evaluation = [r for r in rows if r['split'] == 'evaluation']
    bycase = {(r['block_index'], r['mode'], r['seed'], r['policy']): r for r in evaluation}
    paired = []
    for r in evaluation:
        b = bycase[(r['block_index'], r['mode'], r['seed'], 'fixed_10')]
        paired.append({'policy': r['policy'], 'block_index': r['block_index'], 'mode': r['mode'],
            'seed': r['seed'], 'delta_fixed_10': r['net_trading_quote']-b['net_trading_quote']})
    comparisons = []
    for policy in LABELS:
        differences = [r['delta_fixed_10'] for r in paired if r['policy'] == policy]
        comparisons.append({'policy': policy, 'mean_delta_fixed_10': statistics.fmean(differences),
            'median_delta_fixed_10': statistics.median(differences), 'wins': sum(v > 0 for v in differences),
            'cases': len(differences)})
    (ROOT/'adaptive_comparisons.json').write_text(json.dumps(comparisons, indent=2)+'\n')
    payload = json.loads((ROOT/'artifact.json').read_text())
    before = deepcopy(payload)
    m = payload['manifest']
    datasets = payload['snapshot']['datasets']
    for key in ['blocks', 'charts', 'tables', 'sources']:
        m[key] = [x for x in m.get(key, []) if not x['id'].startswith('adaptive-')]
    payload['sources'] = [s for s in payload['sources'] if not s['id'].startswith('adaptive-')]
    for name in list(datasets):
        if name.startswith('adaptive_'):
            del datasets[name]
    timestamp = datetime.now(timezone.utc).isoformat()
    m['generatedAt'] = payload['snapshot']['generatedAt'] = timestamp
    source = {'id': 'adaptive-results', 'label': 'Adaptive concentration: historical prices with modeled demand',
        'path': 'adaptive_verification.sql', 'query': {'engine': 'SQLite', 'language': 'sql', 'sql': query,
        'executed_at': timestamp, 'description': 'Independent arithmetic from sums of raw replay fee/edge components and denominators. Complete raw rows in adaptive_replays.csv and adaptive_gated_replays.csv; paired comparisons in adaptive_comparisons.json. Methods and predeclared split in adaptive_replay.py and adaptive_plan.json. Gated later-date test is exploratory after primary evaluation.',
        'tables_used': ['totals (from adaptive_replays.csv and adaptive_gated_replays.csv)',
                        'adaptive_replay.py', 'adaptive_plan.json', 'adaptive_selection.json',
                        'adaptive_diagnostics.py', 'market-data/manifest.json'],
        'filters': ['16 separate six-hour blocks across January 8,9,15,19 2025, normalized to each block opening',
            'Training January8-9: 12 fixed/controller candidates, two demand seeds; freeze selection before January15/19 evaluation',
            'Evaluation: four fixed baselines and three selected controllers, three new seeds, four demand schedules',
            'Predeclared gated candidate evaluated after primary failure as exploratory robustness',
            '1m USDT initial capital, fixed center and 100/400-bps widths, same fixed fees and distributed ownership',
            'Decisions use only past prices and executed trades; no trader classifications or rejected intentions',
            'Reserve-preserving curve change, same fee accumulator, immediate profitable arbitrage charged to trading P&L',
            'Governance authorization and protected-depth admission relaxed for economic research'],
        'metric_definitions': ['Mean six-hour trading P&L=(sum LP fees + sum execution edge)/paired cases; remove inventory beta',
            'Immediate switch loss is already inside P&L and arbitrage loss; never subtract it again',
            'Maximum trading drawdown is peak-to-trough cumulative trading P&L within each six-hour run',
            'Coverage signal=(LP fees+positive pre-fee 30-second markouts)/max(1USDT,negative pre-fee markout losses) over trailing five minutes',
            'Extra-cost sensitivity deducts cost per executed switch from the fee wallet, with decisions held fixed',
            'Equal scenario weights are test design, not empirical occurrence probabilities']}}
    feasibility = {'id': 'adaptive-feasibility', 'label': 'Current Dusk concentration update and protected-depth rules',
        'path': 'adaptive_extra_validation.json', 'description':
        'Local source inspected September7,2026. Exact file SHA256 values in adaptive_extra_validation.json. '+
        'Concentration update preserves ordinary reserves and center, reconstructs the curve, then requires covered impairment '+
        'to fit both spendable protected profit and its hard cap. Coverage=125% of positive depth reduction plus1bp of current depth; hard cap=100bps. '+
        'Governance proposal timelock=604800seconds after support approval. Sources: programs/dusk/src/transitions/amm/mod.rs '+
        '(apply_concentrated_curve_parameter_update, covered_impairment_nad, recenter_is_funded), '+
        'programs/dusk/src/transitions/amm/curve.rs (prepare_concentrated_cache_at_point), '+
        'programs/dusk/src/state/market.rs, programs/dusk/src/state/parameter_proposal.rs, programs/dusk/src/constants.rs. '+
        'Real-valued examples in adaptive_switch_examples.csv; these are local-code facts and model calculations, not deployed-market performance.'}
    m['sources'].extend([source, feasibility])
    payload['sources'].extend([source, feasibility])
    datasets['adaptive_verified'] = verified
    datasets['adaptive_comparisons'] = comparisons
    datasets['adaptive_switch_examples'] = extra['examples']
    summary = f'''## Executive Summary
- **Fixed 10× remains the profit benchmark in the expanded test.** It earns {fixed['mean_net_trading']:,.0f} USDT per six-hour evaluation run on average, versus {profit['mean_net_trading']:,.0f} for the selected profitability controller, {flow['mean_net_trading']:,.0f} for the volume controller and {vol['mean_net_trading']:,.0f} for the volatility controller. All include LP-owned base, volatility and outward fees, plus trading losses.
- **Changing concentration can create its own arbitrage loss.** The profitability controller collects more fees but spends {profit['mean_switch_loss']:,.0f} USDT per run on immediate arbitrage after changing the curve. Reserves are preserved; the marginal price changes.
- **A cost gate makes adaptation much more credible, without establishing a profit advantage.** The exploratory cost-gated controller earns {guard['mean_net_trading']:,.0f} USDT, {guard_gap_pct:.1f}% below fixed 10×, and has {drawdown_reduction:.1f}% lower mean trading drawdown. It needs fresh-date validation before selecting it as a risk control.
- **Current Dusk cannot run this minute-scale policy through its existing concentration update.** Governance has a seven-day delay and protected-depth funding constraints. Prices are historical, customer demand is modeled, and the fast controller is a research design.'''
    next(b for b in m['blocks'] if b['id'] == 'summary')['body'] = summary
    gate_wins = next(r['wins'] for r in comparisons if r['policy'] == 'coverage_cost_gate')
    sections = [
        ('answer', f'''## Automatic switching did not beat fixed 10× on trading profit
**The new comparison includes 1,440 primary replays and 96 exploratory gated replays.** In the later-date evaluation, fixed 10× averages {fixed['mean_net_trading']:,.2f} USDT of net trading P&L per six-hour run. The training-selected profitability controller earns {profit['mean_net_trading']:,.2f}, a {gap_pct:.1f}% reduction. Volume-only and volatility-only controls also trail fixed 10× on average.

The chart compares the same 96 price/demand/seed cases per policy at 1m USDT starting capital. The cost-gated result was evaluated after inspecting the primary failure and is explicitly exploratory. These averages give each designed demand scenario equal weight; they do not estimate the frequency of those conditions in a real pool.

**The objective is trading only:** LP-owned fees plus signed execution edge, with between-trade inventory price gains removed. Both dynamic fee components remain enabled for Dusk. Immediate switch losses are already included; subtracting them or LVR again would double-count.''', 'adaptive-results'),
        ('design', '''## Decisions use past data, with reserves carried through every change
**Training uses January 8–9 and evaluation uses January 15 and 19, with new demand seeds.** Each day supplies four separate continuous six-hour blocks at one-second resolution. Every block starts balanced, centered and cold; there is no reset inside the six hours. Twelve fixed/controller candidates train on two seeds, then one controller from each signal family and the best fixed benchmark are frozen before the main evaluation on three seeds. Some hours were inspected in the preceding analysis, so this is not a fully blind validation set.

All adaptive policies start at 5×, observe a ten-minute warmup, decide once per minute and wait at least five minutes between changes. They can move one step among 3×, 5× and 10×. Decisions happen before the next reference tick and arriving orders. Fixed pools run throughout the same warmup; all six hours count toward profit.

The profitability signal uses completed 30-second markouts for all swaps over the preceding five-minute maturity window. It divides fees plus favorable execution markouts by adverse markout losses, with a 1-USDT denominator floor. The selected thresholds are 1.05 and 1.5: choose 3× below the lower threshold, 10× above the upper one and 5× in between. The controller sees no customer/arbitrage labels or rejected intentions. Volume and volatility controls use only past executed turnover and external returns.

Demand is modeled as either steady strong, steady weak, steady cheap execution, or randomly changing half-hour regimes. Prior January 1 CEX clips supply only a size proxy; directions, arrivals and cost tolerance remain explicit assumptions. Every paired policy receives identical offered orders and budgets, with fills responding to its own quotes.''', None),
        ('switching', f'''## Extra fee income did not cover the losses created by reconfiguration
**The profitability controller earns {profit['mean_fees']:,.0f} USDT in LP fees, versus {fixed['mean_fees']:,.0f} at fixed 10×, yet finishes much worse.** It changes concentration {profit['mean_switches']:.1f} times per six-hour run on average. Immediate arbitrage following those changes costs {profit['mean_switch_loss']:,.0f} USDT, compared with a {fixed['mean_net_trading']-profit['mean_net_trading']:,.0f}-USDT total profit gap. The switch-loss comparison identifies an accounting component; it is not a claim that simply adding it back reproduces a no-switch counterfactual.

Changing the curve leaves both token balances intact, but it changes their implied marginal price. Arbitrageurs can trade against that new price. The volatility accumulator is preserved, all fees apply, and greedy profitable arbitrage is settled before the next external-price tick. There is no free recenter, capital injection or inventory reset.

The profitability controller trails fixed 10× on both evaluation days. The January 19 day is worse, but the result is not confined to that day. Delayed profitability can also respond to trades induced by the controller itself; a fee-rich signal does not establish profitable incremental demand.''', 'adaptive-results'),
        ('regimes', '''## Demand conditions still matter for the fixed concentration choice
**Strong fee-tolerant demand favors fixed 10×, while steady weak demand favors fixed 3×.** In the cheap-execution scenario, v1 has the highest mean of the tested fixed curves. The changing-demand scenario also favors fixed 10× in this sample, despite the mix of regimes.

The table reports mean net trading P&L in USDT per six-hour run, with 24 paired cases per demand mode. Weak demand is 100k expected offered volume per hour and a 20-bps median cost budget. Strong demand is 1m/hour and 100 bps; cheap execution is 1m/hour and 50 bps. The changing mode draws those three regimes plus 100k/hour with 100 bps every half hour. These are offered-volume assumptions, not realized captured volume.''', 'adaptive-results'),
        ('gate', f'''## A switch-cost gate preserves most profit and reduces sample drawdowns
**The gated controller averages {guard['mean_net_trading']:,.2f} USDT, versus {fixed['mean_net_trading']:,.2f} at fixed 10×.** It permits a change only when the estimated immediate arbitrage loss is at most 25% of positive net markout P&L over the trailing window. It uses the predeclared 1.1/2.0 coverage thresholds. Average immediate switch loss falls to {guard['mean_switch_loss']:.2f} USDT, although it still executes {guard['mean_switches']:.1f} changes per run.

Mean maximum trading drawdown is {guard['mean_drawdown']:.2f} USDT versus {fixed['mean_drawdown']:.2f} for fixed 10×, a {drawdown_reduction:.1f}% reduction. Its worst six-hour trading outcome is {guard['worst_pnl']:.2f} versus {fixed['worst_pnl']:.2f}. It beats fixed 10× in {gate_wins} of 96 paired cases and remains {guard_gap_pct:.1f}% behind on average.

**This is a possible risk/profit tradeoff, not demonstrated extra profitability.** The candidate was specified before training but evaluated on these later dates after the primary result was inspected. Its gate and coverage thresholds differ from the selected ungated policy, so their difference is not a clean one-factor estimate of the gate's effect. Confirmation requires untouched dates and a preselected risk objective.''', 'adaptive-results'),
        ('costs', '''## Additional transaction costs only widen the profit gap
**All primary numbers already include the endogenous cost of repricing.** The chart below deducts 0, 0.1, 1 and 10 USDT per executed concentration change for additional transaction or operational overhead. It keeps the simulated decisions and reserves fixed and pays that overhead from the external fee wallet.

No tested controller has positive average profit headroom versus fixed 10× at zero added overhead in this evaluation. Therefore none has a positive average break-even extra cost per switch for beating that benchmark. This says nothing about a different objective that explicitly pays for drawdown reduction.''', 'adaptive-results'),
        ('feasibility', '''## The current concentration update cannot execute these fast changes
**The existing path is a governance parameter update with a seven-day timelock after support approval.** A minute-scale controller would need an explicit design for delegated or automatic authority; this analysis changes no protocol code.

Reconfiguration also requires a protected-depth budget. A depth decrease must be covered at 125%, plus a 1-bp guard on current depth, and fit a hard cap of 1% of depth. At centered unchanged reserves, moving from 10× to 5× requires 62.51% of current depth under this formula; moving from 10× to 3× requires 87.51%. Both exceed that hard cap. Distributing fees to LPs does not automatically create the in-pool protected-depth budget.

The economic replays deliberately relax these admission rules so the switching hypothesis can be tested. Reserve conservation and repricing loss are modeled, but those results are not executable current-program returns. Changing how depth protection treats a different curve shape is a separate design question; bypassing the guard is not established as safe.''', 'adaptive-feasibility'),
        ('compute', f'''## The arithmetic is feasible off chain; protocol integration remains unmeasured
**The local research engine completed 1,440 primary replays in about {json.loads((ROOT/'adaptive_validation.json').read_text())['elapsed_seconds']:.0f} seconds.** A small warm Python benchmark rebuilt a reserve-preserving curve in roughly {extra['python_reshape_median_ms']:.3f} ms per call. These are local CPU observations, not Solana compute-unit measurements or a production latency guarantee.

The policy needs an external price reference and delayed swap markouts. Its work scales with observed swaps; the queues store pending 30-second markouts and recent five-minute observations. Pool fee revenue or its internal volatility accumulator alone does not identify toxic flow. An implementation must address reference quality, manipulation and the cost of keeping these observations.

Current Rust curve reconstruction already uses a bounded branch selection and quadratic-root approach. The new economic risk lies in deciding when a change is worth its repricing loss, along with authorization and protected-depth accounting. No automatic controller, integer parity or compute-unit test was implemented here.''', None),
        ('decision', '''## Keep fixed 10× as the profit benchmark and test cost-aware adaptation for risk
**The evidence supports a fixed benchmark and a narrowly defined next experiment.** Fixed 10× leads this test grid on mean trading profit. Fixed 3× remains relevant for weak demand, and cheap execution can favor v1. A lower concentration should be selected with the demand regime and execution objective in view.

For adaptive concentration, take only the cost-gated candidate forward to fresh dates, with a risk objective declared before testing—for example, acceptable trading drawdown at a specified profit concession. Compare it with fixed 3×, 5× and 10× again. Require causal signals, an estimate of repricing cost, and enough evidence that the incremental benefit can pay that cost.

The current sample cannot establish real-world demand capture, market-frequency weights, robustness to strategic manipulation, or a production policy. It has four historical days, two later-date evaluation days and modeled customer demand. Protected-depth semantics and controller authority remain separate unresolved design requirements.''', None),
    ]
    blocks = [{'id': 'adaptive-'+identity, 'type': 'markdown', 'body': body,
               **({'sourceId': source_id} if source_id else {})} for identity, body, source_id in sections]
    profit_chart = [{**get(p), 'net_pnl': get(p)['mean_net_trading']} for p in LABELS]
    datasets['adaptive_profit_chart'] = profit_chart
    cost_labels = {'fixed_10': 'Fixed 10×', 'coverage_150': 'Profit',
                   'flow_050': 'Volume', 'volatility_100': 'Volatility', 'coverage_cost_gate': 'Gated*'}
    cost_chart = [{'label': cost_labels[p], 'overhead': cost,
        'mean_net_pnl': get(p)['mean_net_trading']-cost*get(p)['mean_switches']}
        for p in ['fixed_10', 'coverage_150', 'flow_050', 'volatility_100', 'coverage_cost_gate']
        for cost in [0, .1, 1, 10]]
    datasets['adaptive_cost_chart'] = cost_chart
    for identity, title, kind, dataset, x, y, color, xtitle, ytitle in [
        ('answer', 'Mean net trading P&L per six-hour evaluation run', 'bar', 'adaptive_profit_chart',
         'label', 'net_pnl', None, 'Policy', 'Net trading P&L (USDT)'),
        ('costs', 'Net trading P&L after additional cost per change', 'line', 'adaptive_cost_chart',
         'overhead', 'mean_net_pnl', 'label', 'Additional USDT per concentration change', 'Mean six-hour P&L (USDT)')]:
        encodings = {'x': {'field': x, 'type': 'nominal' if kind == 'bar' else 'quantitative'},
                     'y': {'field': y, 'type': 'quantitative'}}
        if color:
            encodings['color'] = {'field': color, 'type': 'nominal'}
        m['charts'].append({'id': 'adaptive-'+identity, 'title': title, 'type': kind,
            'intent': 'comparison', 'question': title, 'rationale':
            'Bars compare discrete policies on a zero-based common unit. Cost sensitivity uses a numeric axis and fixed case weights; no time-series inference.',
            'dataset': dataset, 'sourceId': 'adaptive-results', 'layout': 'full', 'encodings': encodings,
            'xAxisTitle': xtitle, 'yAxisTitle': ytitle, 'valueFormat': 'number'})
        index = next(i for i,b in enumerate(blocks) if b['id'] == 'adaptive-'+identity)
        blocks.insert(index+1, {'id': 'adaptive-'+identity+'-chart', 'type': 'chart', 'chartId': 'adaptive-'+identity})
    lookup = [{'mode': MODES[mode], **{p: get(p, mode)['mean_net_trading'] for p in LABELS}}
              for mode in ['strong', 'weak', 'cheap', 'changing']]
    datasets['adaptive_demand_lookup'] = lookup
    m['tables'].append({'id': 'adaptive-regimes', 'title': 'Mean six-hour trading P&L by demand scenario',
        'dataset': 'adaptive_demand_lookup', 'sourceId': 'adaptive-results', 'layout': 'full',
        'density': 'spacious', 'columns': [{'field': 'mode', 'label': 'Demand'},
            *[{'field': p, 'label': label, 'format': 'number'} for p,label in LABELS.items()]]})
    index = next(i for i,b in enumerate(blocks) if b['id'] == 'adaptive-regimes')
    blocks.insert(index+1, {'id': 'adaptive-regimes-table', 'type': 'table', 'tableId': 'adaptive-regimes'})
    index = next(i for i,b in enumerate(m['blocks']) if b['id'] == 'definitions')+1
    m['blocks'][index:index] = blocks
    for key in ['blocks', 'charts', 'tables', 'sources']:
        current = {b['id']: b for b in m[key]}
        for old in before['manifest'].get(key, []):
            if old['id'] != 'summary' and not old['id'].startswith('adaptive-'):
                assert current[old['id']] == old
    for name, dataset in before['snapshot']['datasets'].items():
        if not name.startswith('adaptive_'):
            assert datasets[name] == dataset
    assert sum(len(s.get('query', {}).get('sql') or '') for s in m['sources']) < 200000
    (ROOT/'artifact.json').write_text(json.dumps(payload, indent=2)+'\n')
    exact = '| Policy | Mean net P&L | Mean immediate switch loss | Mean max drawdown |\n|---|---:|---:|---:|\n'
    exact += '\n'.join(f"| {LABELS[p]} | {get(p)['mean_net_trading']:,.2f} | {get(p)['mean_switch_loss']:,.2f} | {get(p)['mean_drawdown']:,.2f} |" for p in LABELS)
    (ROOT/'ADAPTIVE_CONCENTRATION.md').write_text('# Adaptive concentration: costs determine viability\n\n'+summary+'\n\n'+
        exact+'\n\nAll values in USDT per six-hour run; 1m initial capital, 96 paired evaluation cases. Cost-gated result is exploratory.\n\n'+
        '\n\n'.join(body for _,body,_ in sections)+'\n\n## Reproduce\n\n```sh\n'+
        'python3 analysis/capital-efficiency/adaptive_replay.py --workers 4\n'+
        'python3 analysis/capital-efficiency/adaptive_diagnostics.py\n'+
        'python3 analysis/capital-efficiency/adaptive_report.py\n```\n\n'+
        'Full report: [report.html](report.html). Plan and selection: `adaptive_plan.json`, `adaptive_selection.json`. '+
        'Raw rows: `adaptive_replays.csv`, `adaptive_gated_replays.csv`. '+
        'Checks: `adaptive_validation.json`, `adaptive_extra_validation.json`, `adaptive_verification.sql`. '+
        'Publisher URLs and checksums: `market-data/manifest.json`. No program code changed.\n')
    validation = json.loads((ROOT/'adaptive_validation.json').read_text())
    validation['exploratory_gated_replays'] = len(gated['rows'])
    validation['report'] = {'source_arithmetic': 'Independent SQLite recomputation reconciles all report means',
        'preservation': 'Every earlier block except executive summary, chart, table, source and dataset preserved exactly',
        'delivery': 'html', 'portable_verification': 'pending',
        'chart_map': ['Discrete policy means: full-width zero-based bar',
                      'Additional switch-cost sensitivity: numeric line plot',
                      'Demand scenario exact lookup: table; all policies use matched cases']}
    (ROOT/'adaptive_validation.json').write_text(json.dumps(validation, indent=2)+'\n')
    return payload


def notebook():
    path = ROOT/'analysis.ipynb'
    nb = json.loads(path.read_text())
    nb['cells'] = [c for c in nb['cells'] if not c.get('id', '').startswith('adaptive-')]
    cells = [
        ('adaptive-intro', 'markdown', '## Adaptive concentration\nCausal controller experiment with reserve-preserving changes, repricing losses, frozen training selection, and explicitly exploratory cost-gated evaluation. Prices are historical; demand is modeled. Governance admission is relaxed for economic research; no program behavior changed.'),
        ('adaptive-results', 'code', "from pathlib import Path\nimport json\nroot=Path.cwd()\nif not (root/'adaptive_results.json').exists(): root=root/'analysis'/'capital-efficiency'\ndata=json.loads((root/'adaptive_results.json').read_text())\ngated=json.loads((root/'adaptive_gated_results.json').read_text())\nprint('Frozen selection:',data['selected'])\nfor r in data['summary']+gated['summary']:\n    if r['split']=='evaluation' and r['mode']=='all':\n        print(r['policy'], 'net',round(r['mean_net_trading_quote'],2),'switch loss',round(r['mean_switch_loss_quote'],2),'drawdown',round(r['mean_max_trading_drawdown_quote'],2))"),
        ('adaptive-audit', 'code', "import sqlite3\nconn=sqlite3.connect(':memory:');conn.row_factory=sqlite3.Row\nverified=[dict(r) for r in conn.execute((root/'adaptive_verification.sql').read_text())]\nconn.close()\nfor r in verified:\n    source=next(v for v in data['summary']+gated['summary'] if all(v[k]==r[k] for k in ['split','mode','policy']))\n    assert abs(source['mean_net_trading_quote']-r['mean_net_trading'])<1e-6\nprint('Independent SQLite means reconciled:',len(verified))\nprint(json.loads((root/'adaptive_extra_validation.json').read_text())['dense_adaptive_hour'])"),
        ('adaptive-limits', 'markdown', '### Interpretation\nMean profit favors fixed10× on this designed grid. The exploratory cost gate trades about2% mean profit for lower sample drawdown; it needs fresh-date validation and an explicit risk objective. Neither minute-scale authority nor protected-depth admission is implemented. The notebook cells are captured sequential Python executions, not Jupyter kernel certification.')]
    namespace = {'__name__': '__main__'}
    count = max(c.get('execution_count') or 0 for c in nb['cells'])
    for identity, kind, body in cells:
        cell = {'id': identity, 'cell_type': kind, 'metadata': {}, 'source': body.splitlines(keepends=True)}
        if kind == 'code':
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                exec(compile(body, identity, 'exec'), namespace)
            count += 1
            cell.update(execution_count=count, outputs=[{'output_type': 'stream', 'name': 'stdout',
                                                        'text': output.getvalue().splitlines(keepends=True)}])
        nb['cells'].append(cell)
    path.write_text(json.dumps(nb, indent=2)+'\n')


if __name__ == '__main__':
    payload = build()
    notebook()
    print('Complete report:', len(payload['manifest']['blocks']), 'blocks;',
          len(payload['manifest']['charts']), 'charts;', len(payload['manifest']['tables']), 'tables.')
