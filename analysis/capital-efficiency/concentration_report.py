"""Append the fixed-fee sweep to the complete existing analytics report."""
from copy import deepcopy
from datetime import datetime, timezone
from pathlib import Path
import contextlib
import io
import json
import math
import sqlite3

ROOT=Path(__file__).resolve().parent


def extend_artifact(payload, root=ROOT):
    data=json.loads((root/'concentration_results.json').read_text())
    parameters=data['parameters']
    from concentration_sweep import FIXED_FEES, AMPLIFICATIONS
    from dataclasses import asdict
    assert parameters['fees']==asdict(FIXED_FEES)
    assert parameters['amplifications']==AMPLIFICATIONS
    assert parameters['capital_usd']==1000000 and parameters['core_bps']==100 and parameters['fade_bps']==400
    assert parameters['states']=={'quiet':0,'cool':.0025,'warm':.005,'hot':.01}
    assert parameters['path_order_seed']==73021 and parameters['path_routing_limit_bps']==150
    before=deepcopy(payload)
    manifest=payload['manifest']
    for key in ['blocks','sources','charts','tables']:
        manifest[key]=[a for a in manifest.get(key,[]) if not a['id'].startswith('concentration-')]
    payload['sources']=[s for s in payload['sources'] if not s['id'].startswith('concentration-')]
    datasets=payload['snapshot']['datasets']
    for key in list(datasets):
        if key.startswith('concentration_'):
            del datasets[key]
    timestamp=datetime.now(timezone.utc).isoformat()
    manifest['generatedAt']=payload['snapshot']['generatedAt']=timestamp

    # Compute displayed metrics independently in runnable SQLite from raw model
    # outputs. Keep original Python sources, rows and provenance available.
    rows=[]
    for r in data['concentration_execution']:
        rows.append(('capital',r['profile'],r['amplification'],r['state'],None,r['notional_usd'],r['capital_usd'],None,None,r['output'],r['matched_capital_usd'],None,None,None))
    for r in data['concentration_matched']:
        rows.append(('risk',r['profile'],r['amplification'],r['state'],None,10000,r['capital_usd'],-r['curve_shortfall_usd'],r['lp_fee_distributed_usd'],None,None,None,None,r['external_jump']))
    for r in data['concentration_paths']:
        rows.append(('paths',r['profile'],r['amplification'],None,r['path'],r['customer_notional_usd'],r['capital_usd'],r['pre_fee_execution_edge_usd'],r['lp_fees_usd'],None,None,r['customer_fills'],r['customer_volume_usd'],None))
    columns='kind,profile,amplification,state,path,notional,capital,pre_fee_edge,lp_fees,output,matched_capital,fills,customer_volume,jump'
    def literal(v):
        if v is None: return 'NULL'
        if isinstance(v,str): return "'"+v.replace("'","''")+"'"
        return repr(v)
    query=('-- Synthetic outputs from concentration_sweep.py; actual independent metric calculation.\nWITH modeled('+columns+') AS (VALUES\n'+
           ',\n'.join('('+','.join(literal(v) for v in r)+')' for r in rows)+')\n'+
           'SELECT *, pre_fee_edge+lp_fees AS net_trading_usd, -(pre_fee_edge+lp_fees) AS net_loss_usd,\n'+
           '10000*(pre_fee_edge+lp_fees)/capital AS net_trading_bps,\n'+
           '10000*(notional/output-1) AS cost_bps, 1-matched_capital/capital AS capital_saved\n'+
           'FROM modeled ORDER BY kind,amplification,state,path,notional,jump;\n')
    (root/'concentration_verification.sql').write_text(query)
    conn=sqlite3.connect(':memory:')
    conn.row_factory=sqlite3.Row
    verified=[dict(r) for r in conn.execute(query)]
    conn.close()
    for r in verified:
        if r['kind']=='capital':
            original=next(x for x in data['concentration_execution'] if x['amplification']==r['amplification'] and x['state']==r['state'] and x['notional_usd']==r['notional'])
            assert abs(r['cost_bps']-original['average_cost_bps'])<1e-7
            assert r['capital_saved'] is None and original['capital_saved_fraction'] is None or math.isclose(r['capital_saved'],original['capital_saved_fraction'],abs_tol=1e-8)
        elif r['kind']=='risk':
            original=next(x for x in data['concentration_matched'] if x['amplification']==r['amplification'] and x['state']==r['state'] and x['external_jump']==r['jump'])
            assert abs(r['net_loss_usd']-original['lp_loss_distributed_usd'])<1e-7
        else:
            original=next(x for x in data['concentration_paths'] if x['amplification']==r['amplification'] and x['path']==r['path'] and x['customer_notional_usd']==r['notional'])
            assert abs(r['net_trading_usd']-original['net_trading_usd'])<1e-6
    source={'id':'concentration-sweep','label':'Fixed-fee concentration experiments and metric cross-check',
        'path':'concentration_verification.sql','query':{'engine':'SQLite','language':'sql','sql':query,'executed_at':timestamp,
        'description':'Actual independent metric recomputation from synthetic source-derived curve/fee outputs. Methods: concentration_sweep.py. Complete raw data: concentration_results.json. Code fingerprints: concentration_validation.json. This is not observed market data.',
        'tables_used':['modeled (inline synthetic outputs)','concentration_results.json','concentration_sweep.py'],
        'filters':['Twelve Dusk amplifications from 1.25x to 50x; v1 base-fee baseline',
            'Fixed 30bps base, distance coefficient 10, volatility coefficient 1, 200bps cap per surcharge, 60s half-life',
            '100bps core plus 400bps shoulder; fixed center; distributed dynamic fees; 20% protocol share of base',
            'No hLP, borrowing, retention, compounding, launch fees or fee-on-output',
            'Each path: 1 hour, 30s observations, cold start, 60 customer intentions, seed 73021, 150bps execution limit'],
        'metric_definitions':['Net trading = LP-owned fees + signed pre-fee execution edge at each current external mark',
            'Net loss = -net trading; LVR is not subtracted again',
            'Trading bps use each pool initial capital and exclude inventory-price gains between trades',
            'Required capital matches v1 output for the same notional; saving = 1-required/$1m',
            'Null capital means unattainable fee floor; initial accumulator is not annualized volatility']}}
    manifest['sources'].append(source)
    payload['sources'].append(source)
    datasets['concentration_verified']=verified

    def ex(a,state='warm'):
        return next(r for r in verified if r['kind']=='capital' and r['amplification']==a and r['state']==state and r['notional']==10000)
    def risk(a,state='warm'):
        return next(r for r in verified if r['kind']=='risk' and r['amplification']==a and r['state']==state and r['jump']==.05)
    def path(a,name,flow):
        return next(r for r in verified if r['kind']=='paths' and r['amplification']==a and r['path']==name and r['notional']==flow)
    e5,e10=ex(5),ex(10)
    r5,r10=risk(5),risk(10)
    same_risk=[next(r['lp_loss_distributed_usd'] for r in data['concentration_bursts'] if r['amplification']==a and r['state']=='warm' and r['external_jump']==.05) for a in [5,10]]
    summary=f'''## Executive Summary
- **Retain 3×, 5× and 10× as contrasting candidates; no single optimum is established.** Twelve Dusk concentrations were tested with identical fee parameters and widths. Trading outcomes depend on order size, routing tolerance and the price path.
- **Execution gains diminish as concentration increases.** A warm $10k buy matches v1 using ${e5['matched_capital']:,.0f} at 5× and ${e10['matched_capital']:,.0f} at 10×. The extra capital saving is {(e10['capital_saved']-e5['capital_saved'])*100:.1f} percentage points, while same-capital warm +5% jump loss rises from ${same_risk[0]:.2f} to ${same_risk[1]:.2f}.
- **More fee-paying demand can justify more concentration; volatility alone does not.** Synthetic one-hour paths count customer and arbitrage trading P&L after LP fees. Large-order choppy flow rewards high concentration in some cases; quiet-to-jump paths can leave the same configuration much worse off.
- **Some execution targets cannot be reached at any concentration.** With a warm signal, the minimum base-plus-volatility all-in cost is about 80.24 bps, above v1's 50.09-bps $1k quote. These are modeled conditional results, not expected market returns.'''
    next(b for b in manifest['blocks'] if b['id']=='summary')['body']=summary
    sections=[
        ('setup','''## Fix fee parameters and vary concentration
**Only amplification changes across Dusk configurations:** 1.25×, 1.5×, 2×, 3×, 4×, 5×, 7.5×, 10×, 15×, 20×, 30× and 50×. Every configuration keeps the 100-bps core, 400-bps additional shoulder and a positive full-range tail. Fixed fees: 30-bps base, outward coefficient 10, volatility coefficient 1, 200-bps cap per surcharge, and 60-second volatility half-life. Fixing parameters does not fix realized fees: each fee still responds to its trade and prior observed pool-price movements.

Equal-capital tests start with $1m ordinary reserves. Equal-execution tests resize each independent pool to match v1's order output, then recompute shock losses on that actual capital. All surcharges are distributed to LPs; the protocol takes 20% of the base fee. Widths, center and concentration remain fixed throughout each path.

**Trading-only P&L = LP fees plus signed execution edge against the external price at each trade.** Both customer and arbitrage trades count. Inventory gains caused by external-price moves between trades are removed and reconciled using an independent endpoint inventory ledger. These are event losses and synthetic one-hour results, without an APR interpretation.'''),
        ('capital',f'''## Capital savings flatten as concentration rises
**5× captures much of the execution benefit under these fees.** The warm $10k order needs ${ex(3)['matched_capital']:,.0f} at 3×, ${e5['matched_capital']:,.0f} at 5×, ${e10['matched_capital']:,.0f} at 10× and ${ex(50)['matched_capital']:,.0f} at 50×, versus $1m for v1. The target all-in cost is 230.09 bps.

Quiet and warm are separate conditional accumulator states, 0 and 0.005. Quiet still pays the outward surcharge. These labels do not specify the variance of the next price move. The chart shows 1.25–20× to keep the lower part of the sweep readable; complete CSVs include 30× and 50×.'''),
        ('risk',f'''## Less capital lowers dollar exposure, but loss per capital still rises
**At matched execution, 5× and 10× have different jump exposure.** The warm +5% same-slot greedy-arbitrage loss is ${r5['net_loss_usd']:.2f} at 5× and ${r10['net_loss_usd']:.2f} at 10×, versus ${risk(1)['net_loss_usd']:.2f} for v1. Those losses are {-r5['net_trading_bps']:.2f}, {-r10['net_trading_bps']:.2f} and {-risk(1)['net_trading_bps']:.2f} bps of each pool's own capital. A lower dollar loss on a smaller pool does not establish a better return per dollar.

Both surcharges are included and affect the arbitrageur's chosen trade size. Each burst allows repeated immediate-profit-maximizing trades, updating the volatility signal. It is not globally optimized strategic splitting. The chart recomputes losses on capital matched under the same initial fee state; v1's dollar benchmark is $275.90. Later decay and trading appear in the one-hour paths below.'''),
        ('paths',f'''## Trading profits depend on the path and on which orders fill
**The paths include all trading profits and losses.** Each run lasts one synthetic hour, starts cold and centered with $1m, samples external prices every 30 seconds, and offers the same 60 seeded buy/sell intentions. An order fills only when all-in cost is at most 150 bps versus the current external price. Arbitrage runs before and after accepted orders; both fees follow the pool's own evolving state.

The table uses $2k per intended order ($120k offered per hour). At a flat external price, v1 earns ${path(1,'flat',2000)['net_trading_usd']:.2f} and Dusk 5× earns ${path(5,'flat',2000)['net_trading_usd']:.2f}. With a quiet-to-5% jump, v1 earns ${path(1,'quiet_then_5pct_jump',2000)['net_trading_usd']:.2f} while Dusk 5× loses ${-path(5,'quiet_then_5pct_jump',2000)['net_trading_usd']:.2f}. Both surcharges are active, but concentration does not win on every path.

The slow trend compounds evenly to +5%; the jump reaches +5% halfway through and stays there. The choppy path completes ten smooth cycles around the center with roughly ±1% price movement. Fills are allowed to differ between pools; these are common opportunities, not identical forced executions. Runs without customer orders isolate adverse selection.'''),
        ('flow',f'''## High concentration can pay when it captures enough large orders
**Large-order access changes the ranking.** On the choppy path with $10k intended orders, 5× fills {path(5,'choppy_1pct',10000)['fills']} of 60 and earns ${path(5,'choppy_1pct',10000)['net_trading_usd']:.2f}; 50× fills {path(50,'choppy_1pct',10000)['fills']} and earns ${path(50,'choppy_1pct',10000)['net_trading_usd']:.2f}. V1 fills none at the 150-bps limit and loses ${-path(1,'choppy_1pct',10000)['net_trading_usd']:.2f} to arbitrage. This is an explicit demand assumption, not evidence that actual customers supply that volume.

With $2k orders on the same price path, 10× earns ${path(10,'choppy_1pct',2000)['net_trading_usd']:.2f} while 50× earns ${path(50,'choppy_1pct',2000)['net_trading_usd']:.2f}. The chart changes order size while keeping fee parameters and the external path fixed. Each point includes fees and signed trading edge.'''),
        ('sensitivity','''## Routing assumptions change the winner
**Exact optima are fragile under this demand model.** A sensitivity grid tests v1 and Dusk at 2×, 3×, 4×, 5×, 10×, 20× and 50×. It uses routing limits of 100 and 300 bps with the original order sequence, plus two additional order-direction seeds at 150 bps: 256 additional runs, with all fee parameters unchanged.

For $10k orders on the choppy path, 50× leads the original 150-bps run and both alternate sequences. Tightening the limit to 100 bps makes 4× lead the sensitivity grid; loosening it to 300 bps makes 10× lead. Flat-price, smaller-order rankings also vary. Hard routing thresholds produce discontinuous fills, so these are scenario winners, not smoothly fitted optimal parameters.

**Retain 3×, 5× and 10× for the next calibrated comparison:** a lower-exposure candidate, an intermediate candidate with substantial execution gains, and a candidate testing whether extra captured demand pays for larger jump losses. Keep 20–50× as stress cases. This is an experimental shortlist, not a deployment recommendation.'''),
        ('floor','''## Concentration cannot remove the minimum fee toll
**A warm $1k order cannot match v1 at any tested concentration, or even at infinite depth in this model.** At an accumulator of 0.005, base plus volatility takes approximately 79.60 bps of gross input. Expressed as all-in cost per unit received, that is 80.24 bps before outward surcharge or price impact. V1's $1k quote costs 50.09 bps including impact. The cool and hot states also fail this target.

These cases are recorded as `unattainable_fee_floor`, with null capital rather than a fabricated large-capital solution. The fees stay unchanged as requested. A later small-order optimization would need to revisit fee settings or the execution target.'''),
        ('limits','''## Calibrate demand before testing an automatic concentration rule
**This sweep supports conditional concentration choices, not a rule that higher volatility should automatically increase concentration.** Next compare the shortlisted fixed configurations using realistic order intentions and external-price updates, then test an explicit controller against those fixed baselines. Changing amplification can move the quoted price and transfer value; that economic cost belongs in the controller comparison.

The model uses real-valued input-denominated fees; no integer program parity or compute-unit claim is made. Both pools distribute fees without compounding. No hLP, debt, retained fees or center movement is included. Greedy arbitrage observes these synthetic prices every 30 seconds, which is not a Solana block-time model. Price movements within those intervals are unobserved. Fees paid in base are valued as if immediately converted at the external mark.

The scenarios have no estimated occurrence probabilities. Three order sequences and three cost limits are sensitivity tests, not confidence intervals. Trading-only P&L excludes directional inventory gains and does not subtract LVR again. The main unresolved quantity is actual profitable customer flow attracted by each concentration relative to adverse selection.''')]
    blocks=[]
    for identity,body in sections:
        block={'id':'concentration-'+identity,'type':'markdown','body':body}
        if identity in ['capital','risk','paths','flow']:
            block['sourceId']='concentration-sweep'
        blocks.append(block)
    chart_specs=[
        ('capital','Capital required to match v1’s $10,000 buy','matched_capital','state','Required initial capital (USD)',
         [r for r in verified if r['kind']=='capital' and r['notional']==10000 and r['state'] in ['quiet','warm'] and 1<r['amplification']<=20]),
        ('risk','LP loss after a +5% jump at matched execution','net_loss_usd','state','Net LP loss per event (USD)',
         [r for r in verified if r['kind']=='risk' and r['jump']==.05 and r['state'] in ['quiet','warm'] and 1<r['amplification']<=20]),
        ('flow','Net trading P&L on the choppy path by order size','net_trading_usd','order_size','One-hour net trading P&L (USD)',
         [{**r,'order_size':f"${r['notional']:,.0f} per order"} for r in verified if r['kind']=='paths' and r['path']=='choppy_1pct' and r['notional']>0 and r['amplification']>1])]
    for identity,title,y,color,unit,chart_rows in chart_specs:
        dataset='concentration_'+identity+'_chart'
        datasets[dataset]=chart_rows
        manifest['charts'].append({'id':'concentration-'+identity,'title':title,'type':'line','intent':'comparison',
            'question':title,'rationale':'Numerically spaced amplification values show a parameter sweep, not time. State or order-size groups are identified by color and legend.',
            'dataset':dataset,'sourceId':'concentration-sweep','layout':'full',
            'encodings':{'x':{'field':'amplification','type':'quantitative'},'y':{'field':y,'type':'quantitative'},'color':{'field':color,'type':'nominal'}},
            'xAxisTitle':'Dusk amplification (×)','yAxisTitle':unit,'valueFormat':'number'})
        i=next(i for i,b in enumerate(blocks) if b['id']=='concentration-'+identity)
        blocks.insert(i+1,{'id':'concentration-'+identity+'-chart','type':'chart','chartId':'concentration-'+identity})
    lookup=[]
    path_names=['flat','slow_5pct_trend','quiet_then_5pct_jump','choppy_1pct']
    for a in [1,2,3,5,10,20,50]:
        lookup.append({'profile':'v1' if a==1 else f'Dusk {a}×','amplification':a,
                       **{name:path(a,name,2000)['net_trading_usd'] for name in path_names}})
    datasets['concentration_path_lookup']=lookup
    manifest['tables'].append({'id':'concentration-paths','title':'One-hour net trading P&L with $2,000 customer orders',
        'dataset':'concentration_path_lookup','sourceId':'concentration-sweep','defaultSort':{'field':'amplification','direction':'asc'},
        'columns':[{'field':'profile','label':'Configuration'},{'field':'amplification','label':'A (×)','format':'number'},
                   *[{'field':field,'label':label,'format':'currency'} for field,label in zip(path_names,['Flat','Slow +5%','Quiet → +5% jump','Choppy ±1%'])]]})
    i=next(i for i,b in enumerate(blocks) if b['id']=='concentration-paths')
    blocks.insert(i+1,{'id':'concentration-paths-table','type':'table','tableId':'concentration-paths'})
    i=next(i for i,b in enumerate(manifest['blocks']) if b['id']=='execution')
    manifest['blocks'][i:i]=blocks
    # Earlier sections, charts and tables are retained exactly; the executive
    # summary is the only earlier block changed to reflect the new evidence.
    for key in ['blocks','charts','tables']:
        current={a['id']:a for a in manifest[key]}
        for old in before['manifest'].get(key,[]):
            if old['id']!='summary' and not old['id'].startswith('concentration-'):
                assert current[old['id']]==old
    table=('| Configuration | Required capital | Capital saved | Loss on resized pool | Loss / own capital |\n'
           '|---|---:|---:|---:|---:|\n'+
           '\n'.join(f"| {'v1' if a==1 else str(a)+'×'} | ${ex(a)['matched_capital']:,.0f} | {ex(a)['capital_saved']:.1%} | ${risk(a)['net_loss_usd']:.2f} | {-risk(a)['net_trading_bps']:.2f} bps |" for a in [1,2,3,5,10,20,50]))
    (root/'CONCENTRATION_SWEEP.md').write_text('# Fixed-fee concentration sweep\n\n'+summary+'\n\n'+
        '\n\n'.join(body for _,body in sections)+'\n\n## Exact lookup: warm $10k execution and warm +5% jump\n\n'+table+
        '\n\n## Reproduce\n\n```sh\npython3 analysis/capital-efficiency/concentration_sweep.py\npython3 analysis/capital-efficiency/concentration_report.py\n```\n\n'
        'The first command calculates scenarios and validation. The second preserves and extends the complete report, recomputes chart metrics in SQLite, and adds executed notebook cells. '
        'Raw outputs: `concentration_results.json` and `concentration_*.csv`. Source hashes and checks: `concentration_validation.json`.\n')
    notes=json.loads((root/'concentration_validation.json').read_text())
    notes['report']={'delivery':'mcp-app','sql_cross_check':'All capital, risk and path display metrics reconciled',
        'structure':'Existing full report preserved. Updated executive summary; fixed-fee definitions, findings with three charts and a table, shortlist, questions and caveats.',
        'chart_contract':'Numeric amplification axes, grouped state/order-size lines with legends, explicit monetary units; exact path P&L in a table.',
        'visual_qa':'Native validation/rendering requested; direct Codex screenshot inspection unavailable in this session.'}
    (root/'concentration_validation.json').write_text(json.dumps(notes,indent=2)+'\n')
    return payload


def extend_notebook(root=ROOT):
    nb=json.loads((root/'analysis.ipynb').read_text())
    nb['cells']=[c for c in nb['cells'] if not c.get('id','').startswith('conc-')]
    cells=[('conc-intro','markdown','## Fixed-fee concentration sweep\nTwelve Dusk concentrations; fees and widths fixed. See CONCENTRATION_SWEEP.md. Cells below independently recompute execution cases and sample trading paths from either the repository root or analysis directory.'),
        ('conc-setup','code',"from pathlib import Path\nimport sys, json\nroot=Path.cwd()\nif not (root/'concentration_sweep.py').exists():\n    root=root/'analysis'/'capital-efficiency'\nsys.path.insert(0,str(root))\nfrom concentration_sweep import event_experiments, replay, path_inputs\nevents=event_experiments()\nfor row in events['concentration_execution']:\n    if row['state']=='warm' and row['notional_usd']==10000:\n        print(row['profile'],'required capital',round(row['matched_capital_usd']),'saved',round(row['capital_saved_fraction']*100,2))"),
        ('conc-path','code',"for a in [1,3,5,10]:\n    row=replay(a,path_inputs()['choppy_1pct'],2000)\n    assert abs(row['net_trading_usd']-row['ledger_net_trading_usd'])<1e-5\n    print(row['profile'],'net trading',round(row['net_trading_usd'],2),'fills',row['customer_fills'])"),
        ('conc-limits','markdown','### Interpretation\nFixed parameters do not mean fixed realized fees. No single amplification wins across paths and order assumptions. The warm $1k fee floor cannot be solved with concentration. Cells executed sequentially in Python with captured outputs; this does not certify a Jupyter kernel or on-chain program parity.')]
    namespace={'__name__':'__main__'}
    count=max(c.get('execution_count') or 0 for c in nb['cells'])
    for identity,kind,body in cells:
        cell={'id':identity,'cell_type':kind,'metadata':{},'source':body.splitlines(keepends=True)}
        if kind=='code':
            stream=io.StringIO()
            with contextlib.redirect_stdout(stream):
                exec(compile(body,identity,'exec'),namespace)
            count+=1
            cell.update(execution_count=count,outputs=[{'output_type':'stream','name':'stdout','text':stream.getvalue().splitlines(keepends=True)}])
        nb['cells'].append(cell)
    (root/'analysis.ipynb').write_text(json.dumps(nb,indent=2)+'\n')


if __name__=='__main__':
    payload=extend_artifact(json.loads((ROOT/'artifact.json').read_text()))
    (ROOT/'artifact.json').write_text(json.dumps(payload,indent=2)+'\n')
    extend_notebook()
    print('Updated complete report, SQLite checks, companion narrative and executed notebook extension.')
