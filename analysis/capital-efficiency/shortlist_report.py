"""Extend the complete report with historical-price shortlist evidence."""
from collections import Counter,defaultdict
from copy import deepcopy
from datetime import datetime,timezone
from pathlib import Path
import contextlib
import io
import json
import math
import sqlite3
import statistics

ROOT=Path(__file__).resolve().parent
LABELS={'quiet_p10':'Quiet','median_p50':'Typical','volatile_p90':'Volatile','extreme_p99':'Extreme','largest_hourly_move':'Largest move'}


def sql_rows(name,columns,rows,projection):
    def literal(v):
        if isinstance(v,str):return "'"+v.replace("'","''")+"'"
        if v is None:return 'NULL'
        return repr(v)
    query='WITH source_rows('+','.join(columns)+') AS (VALUES\n'+',\n'.join('('+','.join(literal(v) for v in r)+')' for r in rows)+')\nSELECT '+projection+' FROM source_rows;\n'
    (ROOT/(name+'.sql')).write_text(query)
    conn=sqlite3.connect(':memory:');conn.row_factory=sqlite3.Row
    result=[dict(r) for r in conn.execute(query)];conn.close()
    return query,result


def build():
    data=json.loads((ROOT/'shortlist_results.json').read_text())
    quality=json.loads((ROOT/'shortlist_data_quality.json').read_text())
    inputs=json.loads((ROOT/'shortlist_market_inputs.json').read_text())
    validation=json.loads((ROOT/'shortlist_validation.json').read_text())
    assert validation['replays']==1015 and data['parameters']['tolerance_medians_bps']==[20,50,100,200]
    payload=json.loads((ROOT/'artifact.json').read_text());before=deepcopy(payload)
    manifest=payload['manifest'];datasets=payload['snapshot']['datasets']
    for key in ['blocks','charts','tables','sources']:
        manifest[key]=[x for x in manifest.get(key,[]) if not x['id'].startswith('shortlist-')]
    payload['sources']=[x for x in payload['sources'] if not x['id'].startswith('shortlist-')]
    for name in list(datasets):
        if name.startswith('shortlist_'):del datasets[name]
    timestamp=datetime.now(timezone.utc).isoformat()
    manifest['generatedAt']=payload['snapshot']['generatedAt']=timestamp
    # Independent SQL arithmetic starts from raw sums and denominators, not
    # already rounded narrative figures. Raw seed rows remain in the CSV.
    groups=defaultdict(list)
    key_fields=['experiment','window_index','amplification','capital_mode','arb_interval_seconds','demand_target_quote','tolerance_median_bps']
    for r in data['rows']:groups[tuple(r[k] for k in key_fields)].append(r)
    columns=['experiment','window_index','amplification','capital_mode','interval_seconds','target_flow','budget_median','window','capital','seeds','sum_fees','sum_edge','sum_arb_loss','sum_offered','sum_filled']
    totals=[]
    for key,values in groups.items():
        first=values[0]
        totals.append((*key,LABELS[first['window']],first['capital_quote'],len(values),
            sum(r['lp_fees_quote'] for r in values),sum(r['pre_fee_execution_edge_quote'] for r in values),
            sum(r['arb_loss_after_fees_quote'] for r in values),sum(r['offered_volume_quote'] for r in values),sum(r['filled_volume_quote'] for r in values)))
    query,verified=sql_rows('shortlist_verification',columns,totals,
        '*, (sum_fees+sum_edge)/seeds AS mean_net_trading, 10000*(sum_fees+sum_edge)/(seeds*capital) AS mean_trading_bps, '
        'sum_fees/seeds AS mean_fees, sum_arb_loss/seeds AS mean_arb_loss, sum_filled/seeds AS mean_filled, '
        'CASE WHEN sum_offered>0 THEN sum_filled/sum_offered ELSE NULL END AS fill_volume_fraction')
    for r in verified:
        expected=next(x for x in data['summary'] if tuple(x[k] for k in key_fields)==tuple(r[k] for k in ['experiment','window_index','amplification','capital_mode','interval_seconds','target_flow','budget_median']))
        assert abs(r['mean_net_trading']-expected['mean_net_trading_quote'])<1e-6
        assert abs(r['mean_trading_bps']-expected['mean_net_trading_bps'])<1e-7
        r['profile']='v1' if r['amplification']==1 else f"Dusk {r['amplification']}×"
    source={'id':'shortlist-results','label':'Historical-price replay with modeled customer demand',
        'path':'shortlist_verification.sql','query':{'engine':'SQLite','language':'sql','sql':query,'executed_at':timestamp,
        'description':'Independent mean and capital-normalized P&L calculation from sums of per-trade LP fees and execution edge. Raw per-seed replay rows: shortlist_replays.csv. Methods: shortlist_replay.py and fee_optimizer.py. Demand is synthetic; historical price and size-proxy source manifest: market-data/manifest.json.',
        'tables_used':['source_rows (totals from shortlist_replays.csv)','shortlist_replays.csv','shortlist_replay.py','fee_optimizer.py'],
        'filters':['v1 and Dusk 3x,5x,10x; all fee parameters fixed at the previously tested values',
            'Five separate one-hour January 2025 SOL/USDT episodes; each starts centered, balanced and cold',
            'Observed 1-second last-trade prices; modeled Poisson arrivals and fair buy/sell intentions',
            'Sizes bootstrapped without trimming from January 1 CEX aggregate clips; budgets lognormal with median 20/50/100/200bps and log-sigma0.5',
            '100k/1m quote expected offered volume per hour, plus zero-demand controls; five shared demand seeds',
            'Fees distributed; 20% of base to protocol; no compounding, recentering, hLP, debt or adaptive concentration'],
        'metric_definitions':['Mean trading = (sum LP-owned fees + sum signed external-mark execution edge)/number of seeds',
            'Trading bps = mean trading / initial quote capital * 10000; inventory-price gains between trades excluded',
            'Arbitrage loss includes arbitrage after customer impact; it is not independently estimated fee-free LVR',
            'Filled volume fraction = total filled / total offered; target flow is expected offered volume, not realized pool volume',
            'No scenario-frequency weights or annualized returns; units are USDT, not an assumed USD conversion']}}
    # Observed-price chart uses a separate source and preserves actual SOL/USDT
    # levels and time coordinates, not only chart percentages.
    price_rows=[]
    for w in inputs['windows']:
        for sec in range(0,3601,30):
            normalized=1 if sec==0 else w['prices'][sec-1]
            price_rows.append((LABELS[w['window']],w['start_utc'],sec/60,normalized,w['initial_sol_usdt']))
    price_query,price_data=sql_rows('shortlist_price_verification',['window','start_utc','minutes','normalized_price','initial_sol_usdt'],price_rows,
                                 '*, 100*(normalized_price-1) AS price_change_pct, normalized_price*initial_sol_usdt AS sol_usdt')
    price_source={'id':'shortlist-prices','label':'Binance public SOL/USDT price archives, January 2025',
        'href':'https://github.com/binance/binance-public-data/blob/master/README.md',
        'path':'shortlist_price_verification.sql','query':{'engine':'SQLite','language':'sql','sql':price_query,
        'description':'Observed spot last-trade closes. Plot samples every30 seconds from the1-second data used by the replay. Publisher SHA256 checksums verified; archive URLs and hashes in market-data/manifest.json.',
        'tables_used':['SOLUSDT-1m-2025-01.zip','SOLUSDT-1s-2025-01-08.zip','SOLUSDT-1s-2025-01-09.zip','SOLUSDT-1s-2025-01-15.zip','SOLUSDT-1s-2025-01-19.zip'],
        'filters':['January2–31 hourly variance percentiles10,50,90,99 and largest absolute hourly return; chosen independently of LP results'],
        'metric_definitions':['Price change percent=100*(current price/hour opening price-1)','The replay observes every completed second; visual decimation does not set simulation frequency']}}
    for s in [source,price_source]:manifest['sources'].append(s);payload['sources'].append(s)
    datasets['shortlist_verified']=verified;datasets['shortlist_prices']=price_data
    def get(w,a,flow=1000000,budget=100,experiment='primary'):
        return next(r for r in verified if r['window_index']==w and r['amplification']==a and r['target_flow']==flow and r['budget_median']==budget and r['experiment']==experiment)
    winners=Counter()
    for w in range(5):
        for flow in [100000,1000000]:
            for budget in [20,50,100,200]:
                winners[max((get(w,a,flow,budget) for a in [1,3,5,10]),key=lambda x:x['mean_net_trading'])['amplification']]+=1
    paired=[]
    for w in range(5):
        for seed in [101,102,103,104,105]:
            subset=[r for r in data['rows'] if r['experiment']=='primary' and r['window_index']==w and r['seed']==seed and r['demand_target_quote']==1000000 and r['tolerance_median_bps']==100]
            paired.append(next(r['net_trading_quote'] for r in subset if r['amplification']==10)-next(r['net_trading_quote'] for r in subset if r['amplification']==5))
    summary=f'''## Executive Summary
- **10× becomes the leading candidate when substantial demand accepts the fee schedule.** With1m USDT of expected offered volume per hour and a100-bps median customer cost budget,10× has the highest five-seed mean in all five historical-price episodes. It beats5× in {sum(x>0 for x in paired)} of25 paired seed runs, so the advantage is not universal.
- **Lower concentration remains useful when flow or fee tolerance is weak.** At100k offered volume and a20-bps median budget during the−8.08% hour, net trading P&L is {get(4,3,100000,20)['mean_net_trading']:.2f} USDT at3×, {get(4,5,100000,20)['mean_net_trading']:.2f} at5× and {get(4,10,100000,20)['mean_net_trading']:.2f} at10×. These are losses after LP fees.
- **There is no unconditional winner across the demand grid.** Of40 scenario means,10× leads {winners[10]},3× leads {winners[3]},5× leads {winners[5]} and v1 leads {winners[1]}. These counts describe the chosen test grid, not market probabilities.
- **Prices are historical; demand remains modeled.** The1,015 replays use checksum-verified SOL/USDT prices and a prior-day CEX clip-size proxy. Dusk customer capture and fee tolerance are unobserved. No production setting, expected APR or automatic concentration policy is established.'''
    # Keep reader-facing spacing natural even around mathematical notation.
    replacements={'With1m':'With 1m','a100-bps':'a 100-bps','hour,10×':'hour, 10×','beats5×':'beats 5×','of25':'of 25',
                  'At100k':'At 100k','a20-bps':'a 20-bps','the−8.08%':'the −8.08%','at3×':'at 3×','at5×':'at 5×','at10×':'at 10×',
                  'Of40':'Of 40','means,10×':'means, 10×','The1,015':'The 1,015'}
    for a,b in replacements.items():summary=summary.replace(a,b)
    next(b for b in manifest['blocks'] if b['id']=='summary')['body']=summary
    body_sections=[
        ('scope','''## Historical prices replace the invented paths; demand is explicit
**The comparison now uses five historical SOL/USDT hours.** They represent the10th,50th,90th and99th percentiles of hourly realized variance, plus the largest absolute hourly move, among720 hours from January2–31,2025. Selection used external-price statistics before any simulated LP results. Each episode starts centered, balanced and cold at its own opening price; this is not a continuous month-long backtest.

All fee controls remain fixed:30-bps base, outward coefficient10, volatility coefficient1, and200-bps cap per surcharge with a60-second half-life. Dusk uses3×,5× or10× concentration with100/400-bps core/shoulder widths. The baseline is v1 with the same base fee and protocol share. Main runs use1m USDT capital; both surcharge components accrue to LPs.

The simulation uses each completed second's observed price, then processes modeled customer intentions and arbitrage at that boundary. The chart samples every30 seconds for readability; the replay itself uses1-second data. Prices are last-trade proxies from one CEX, not a consolidated efficient mid.''','prices'),
        ('demand',f'''## Use observed clip sizes without calling them Dusk customer demand
**The size proxy comes from {quality['calibration']['agg_trade_clips']:,} aggregated trade clips on January1, before every test episode.** Median notional is {quality['calibration']['p50_clip_quote']:.2f} USDT; the90th percentile is {quality['calibration']['p90_clip_quote']:.2f}, and the99th is {quality['calibration']['p99_clip_quote']:.2f}. The full empirical distribution is sampled without trimming. An aggregate CEX clip is not necessarily a complete customer order and does not identify non-toxic flow.

Customer intentions arrive through a Poisson process calibrated to expected offered volume of100k or1m USDT per hour. Sizes are bootstrapped from that earlier distribution; directions are independently50/50. Each customer draws an individual lognormal cost budget, with median20,50,100 or200 bps and log-standard-deviation0.5. A trade fills only if its all-in quote meets that customer's budget. Actual offered and filled volumes vary by seed, and every concentration receives the same intentions and budgets within a paired case.

These are explicit assumptions about Dusk demand. The observed CEX volume does not establish pool capture, customer fee tolerance, clustered arrivals or directional predictability. The test contains820 primary replays,120 arbitrage-interval checks and75 replays with less starting capital:1,015 total.''',None),
        ('profits',f'''## With enough fee-tolerant demand,10× leads the shortlist
**At1m expected offered volume and a100-bps median budget,10× has the highest mean trading P&L in all five episodes.** Each displayed value averages five independent demand seeds, using1m initial capital and1-second arbitrage opportunities. In the quiet hour,5× earns {get(0,5)['mean_net_trading']:.2f} USDT and10× earns {get(0,10)['mean_net_trading']:.2f}. In the extreme-volatility hour they earn {get(3,5)['mean_net_trading']:.2f} and {get(3,10)['mean_net_trading']:.2f}, respectively.

The quiet-hour mean advantage is small:10× beats5× in only three of five seeds there. Across all five episodes it wins21 of25 paired seed runs. Those are Monte Carlo comparisons under specified demand assumptions, not real-world confidence intervals or probabilities.

**Trading P&L includes every executed customer and arbitrage trade:** LP fees plus signed execution edge at the contemporaneous external reference. Price-change gains and losses on inventory between trades are removed using an independent ledger. Do not interpret these values as total unhedged LP returns, and do not subtract LVR again.''','results'),
        ('tolerance',f'''## Cheap execution demands can reverse the ranking
**High concentration needs customers who will actually pay its resulting quotes.** On the−8.08% hour with1m expected offered volume, a20-bps median budget leaves3× at {get(4,3,1000000,20)['mean_net_trading']:.2f} USDT and10× at {get(4,10,1000000,20)['mean_net_trading']:.2f}. With a100-bps median budget, those become {get(4,3)['mean_net_trading']:.2f} and {get(4,10)['mean_net_trading']:.2f}.

The plotted comparison keeps the same historical hour, expected order volume, sizes, seed set and fee parameters; it changes customer willingness to accept execution cost. Individual budgets are heterogeneous, so a20-bps median does not forbid every trade above20 bps. A portion of customers have larger budgets.

High demand also does not guarantee that Dusk beats v1: at1m offered volume and a50-bps median budget in that hour, v1 earns {get(4,1,1000000,50)['mean_net_trading']:.2f} USDT versus {get(4,10,1000000,50)['mean_net_trading']:.2f} at10×. Across the entire40-cell primary demand grid,10× leads20 cells,3× nine,5× eight and v1 three. The grid is not weighted by estimated market frequencies.''','results'),
        ('capital','''## Smaller initial pools can improve trading profit per capital under the tested demand
**The matched-capital check preserves the same absolute customer opportunities.** Each Dusk pool is resized to match v1's10k USDT buy at the initial cold state. Required capital is approximately726,961 USDT at3×,624,195 at5× and563,311 at10×, versus1m for v1. These are cold-start matches; they differ from the earlier warm-state capital figures because the volatility fee is initially zero.

The table uses1m expected offered volume, a100-bps median budget and the same five seeds. It reports mean net trading bps relative to each pool's own initial capital. Every concentration is replayed on its actual resized curve; no P&L is merely scaled from the equal-capital result.10× has the highest mean trading P&L per capital in all five of these matched-capital cases.

This does not establish equal risk or execution throughout the hour. Later prices, fee signals and quotes diverge, and the smaller pools receive more offered volume relative to their capital. The comparison is conditional on being offered the same absolute intentions.''','results'),
        ('latency','''## The main tradeoff survives the tested timing change
**Arbitrage timing is checked using identical seeds.** The latency extension compares opportunities every1,5 and30 seconds while continuing to observe the same1-second reference prices and processing customer intentions at their scheduled boundaries. The comparison uses the first three seeds,1m expected offered volume and100-bps median budgets for every interval.

For those matched seeds,5× leads in the quiet hour at all three intervals;10× leads in the other four hours at all three intervals. This also illustrates why comparing five seeds at1 second with only three at a slower interval would be misleading. The stored per-seed rows allow exactly paired comparisons.

The numerical optimizer now checks both the uncapped and capped outward-fee regimes. A coarse grid missed a second profit peak in an extreme-volatility replay. The revised solver agrees with an independent1,600-point search on four full extreme-hour replays and60 randomized off-center opportunities. This is research-model computation; no on-chain compute-unit measurement is claimed.''',None),
        ('decision','''## Advance10× for strong demand, retain3× and5× as conditional alternatives
**The evidence favors a conditional shortlist.**10× is the leading candidate when offered volume is substantial and customers accept the fee schedule.3× provides the lower-exposure alternative when demand or fee tolerance is weak.5× remains an intermediate candidate, including the quiet-hour runs where its advantage over10× depends on the order sample.

Before choosing a live parameter or automatic controller, the controlling unknown is the target market's actual demand response: order sizes, available alternative quotes, willingness to pay, directional clustering and arrival intensity. Historical CEX prices and clip sizes do not measure those quantities for Dusk.

An adaptive controller should subsequently be evaluated against these fixed baselines on the same opportunities, including any price movement or value transfer caused by changing amplification. This run does not implement that controller or change protocol parameters.''',None),
        ('quality','''## Data and accounting checks passed, with a defined scope
**The archives are checksum verified and the selected price paths reconcile.** The source check covers44,640 minute rows,345,600 second rows and238,579 prior-day aggregate clips. It checks timestamp units, continuous unique bar keys, OHLC bounds, finite prices, volumes and clip identifiers. All300 selected minute closes and quote volumes reconcile with the corresponding second rows.

Every replay reconciles fee components, customer and arbitrage trading P&L, offered/fill counts, and an independent endpoint inventory ledger. Displayed means are recomputed in SQLite from sums and seed counts. The full raw seed rows, source hashes, source URLs, cost-budget assumptions and executed notebook cells accompany the report.

Limits: one CEX price proxy, five selected independent hours, five demand seeds, and unobserved Dusk customer demand. No regime-frequency weighting, annualized return, continuous-month performance, subsecond execution, strategic transaction splitting, integer program parity, recentering, adaptive amplification, fee compounding, hLP or borrowing is claimed. All monetary values are USDT quote units; no USD peg conversion is performed.''',None)]
    # Avoid dense parameter strings in the reader-facing prose.
    import re
    def spacing(text):
        saved=[]
        def protect(match):
            saved.append(match.group())
            return 'ZZNUMBER'+chr(65+len(saved)-1)+'ZZ'
        text=re.sub(r'(?<![\d,])\d{1,3}(?:,\d{3})+(?![\d,])',protect,text)
        text=re.sub(r'\bv1\b','ZZVONEZZ',text)
        text=re.sub(r'(?<=[A-Za-z])(?=\d)', ' ',text)
        text=re.sub(r'(?<=\d)(?=[A-Za-z])', ' ',text)
        text=re.sub(r',(?=\d)', ', ',text)
        text=re.sub(r'(\d) (st|nd|rd|th)\b',r'\1\2',text)
        text=re.sub(r'\b1 m\b','1 million',text)
        text=re.sub(r'\b100 k\b','100,000',text)
        text=re.sub(r'\b10 k\b','10,000',text)
        text=re.sub(r'(?<=[;:])(?=\S)',' ',text)
        text=re.sub(r'(?<=[A-Za-z×])\.(?=\d)','. ',text)
        text=re.sub(r'(?<=[.!?])\*\*(?=\d)','** ',text)
        text=re.sub(r'(?<=[A-Za-z])(?=−\d)',' ',text)
        text=text.replace('ZZVONEZZ','v1')
        for i,value in enumerate(saved):text=text.replace('ZZNUMBER'+chr(65+i)+'ZZ',value)
        return text
    blocks=[]
    for identity,body,source_key in body_sections:
        block={'id':'shortlist-'+identity,'type':'markdown','body':spacing(body)}
        if source_key:block['sourceId']='shortlist-'+source_key
        blocks.append(block)
    chart_specs=[
        ('scope','Historical SOL/USDT price paths','shortlist_prices','line','minutes','price_change_pct','window','Minutes from episode start','Price change (%)','prices',price_data),
        ('profits','Net trading P&L with1m offered volume and100-bps median budget','shortlist_profit_chart','bar','window','mean_net_trading','profile','Historical episode','Mean one-hour net trading P&L (USDT)','results',
         [r for r in verified if r['experiment']=='primary' and r['target_flow']==1000000 and r['budget_median']==100]),
        ('tolerance','Net trading P&L by customer budget during the largest-move hour','shortlist_budget_chart','line','budget_median','mean_net_trading','profile','Median customer cost budget (bps)','Mean one-hour net trading P&L (USDT)','results',
         [r for r in verified if r['experiment']=='primary' and r['target_flow']==1000000 and r['window_index']==4])]
    for identity,title,dataset,kind,x,y,color,xaxis,yaxis,source_key,rows in chart_specs:
        datasets[dataset]=rows
        manifest['charts'].append({'id':'shortlist-'+identity,'title':spacing(title),'type':kind,'intent':'comparison',
            'question':spacing(title),'rationale':'Explicit comparable scenario groups with visible legends and consistent monetary units; historical paths are kept separate from modeled outcomes.',
            'dataset':dataset,'sourceId':'shortlist-'+source_key,'layout':'full',
            'encodings':{'x':{'field':x,'type':'nominal' if x=='window' else 'quantitative'},'y':{'field':y,'type':'quantitative'},'color':{'field':color,'type':'nominal'}},
            'xAxisTitle':xaxis,'yAxisTitle':yaxis,'valueFormat':'number'})
        index=next(i for i,b in enumerate(blocks) if b['id']=='shortlist-'+identity)
        blocks.insert(index+1,{'id':'shortlist-'+identity+'-chart','type':'chart','chartId':'shortlist-'+identity})
    matched=[]
    for a in [1,3,5,10]:
        sample=get(0,a,experiment='primary' if a==1 else 'matched_capital')
        row={'amplification':a,'profile':'v1' if a==1 else f'Dusk {a}×','capital':sample['capital']}
        for w in range(5):row['window_'+str(w)]=get(w,a,experiment='primary' if a==1 else 'matched_capital')['mean_trading_bps']
        matched.append(row)
    datasets['shortlist_matched_lookup']=matched
    manifest['tables'].append({'id':'shortlist-capital','title':'Mean trading bps on capital matched to the initial10k quote','dataset':'shortlist_matched_lookup','sourceId':'shortlist-results',
        'defaultSort':{'field':'amplification','direction':'asc'},'columns':[{'field':'amplification','label':'A (1=v1)','format':'number'},{'field':'capital','label':'Capital (USDT)','format':'number'},
            *[{'field':'window_'+str(i),'label':label+' (bps)','format':'number'} for i,label in enumerate(LABELS.values())]]})
    index=next(i for i,b in enumerate(blocks) if b['id']=='shortlist-capital')
    blocks.insert(index+1,{'id':'shortlist-capital-table','type':'table','tableId':'shortlist-capital'})
    index=next(i for i,b in enumerate(manifest['blocks']) if b['id']=='concentration-setup')
    manifest['blocks'][index:index]=blocks
    for key in ['blocks','charts','tables']:
        current={b['id']:b for b in manifest[key]}
        for old in before['manifest'].get(key,[]):
            if old['id']!='summary' and not old['id'].startswith('shortlist-'):assert current[old['id']]==old
    summary=spacing(summary);next(b for b in manifest['blocks'] if b['id']=='summary')['body']=summary
    manifest['tables'][-1]['title']=spacing(manifest['tables'][-1]['title'])
    assert sum(len(s.get('query',{}).get('sql') or '') for s in manifest['sources'])<200000
    (ROOT/'artifact.json').write_text(json.dumps(payload,indent=2)+'\n')
    table='| Historical episode | v1 |3× |5× |10× |\n|---|---:|---:|---:|---:|\n'+'\n'.join('|'+LABELS[inputs['windows'][w]['window']]+'|'+'|'.join(f"{get(w,a)['mean_net_trading']:.2f}" for a in [1,3,5,10])+'|' for w in range(5))
    (ROOT/'HISTORICAL_SHORTLIST.md').write_text('# Historical-price shortlist: fixed fees\n\n'+summary+'\n\n'+
        '\n\n'.join(b['body'] for b in blocks if b['type']=='markdown')+
        '\n\n## Exact lookup\n\nMean one-hour net trading P&L in USDT, 1 million capital, 1 million expected offered volume, 100-bps median budget, five seeds.\n\n'+table+
        '\n\n## Reproduce\n\n```sh\npython3 analysis/capital-efficiency/fetch_market_data.py\npython3 analysis/capital-efficiency/shortlist_replay.py --workers 4\npython3 analysis/capital-efficiency/shortlist_report.py\n```\n\n'
        'Public data documentation: https://github.com/binance/binance-public-data/blob/master/README.md. '
        'Archive URLs/checksums: `market-data/manifest.json`. Raw results: `shortlist_replays.csv`; summaries: `shortlist_summary.csv`; checks: `shortlist_validation.json`; data QA: `shortlist_data_quality.json`.\n')
    validation['report']={'source_arithmetic':'Independent SQLite mean, fee and capital-normalized metrics reconcile',
        'delivery':'mcp-app','structure':'Earlier full report preserved; executive summary updated; historical evidence, drivers, matched capital, decision and caveats added',
        'visual_qa':'Native validation and render pending; direct Codex screenshot inspection unavailable in this session'}
    (ROOT/'shortlist_validation.json').write_text(json.dumps(validation,indent=2)+'\n')
    return payload


def notebook():
    path=ROOT/'analysis.ipynb';nb=json.loads(path.read_text())
    nb['cells']=[c for c in nb['cells'] if not c.get('id','').startswith('short-')]
    cells=[('short-intro','markdown','## Historical-price shortlist with modeled demand\nPrices and prior-day clip sizes are observed; Dusk arrivals, direction and customer cost budgets are assumptions. See HISTORICAL_SHORTLIST.md and source checks in market-data/manifest.json.'),
        ('short-results','code',"from pathlib import Path\nimport json,sys\nroot=Path.cwd()\nif not (root/'shortlist_replay.py').exists():root=root/'analysis'/'capital-efficiency'\nsys.path.insert(0,str(root))\nresults=json.loads((root/'shortlist_results.json').read_text())\nquality=json.loads((root/'shortlist_data_quality.json').read_text())\nprint(quality['calibration'])\nfor r in results['summary']:\n    if r['experiment']=='primary' and r['demand_target_quote']==1000000 and r['tolerance_median_bps']==100:\n        print(r['window'],r['amplification'],round(r['mean_net_trading_quote'],2))"),
        ('short-replay','code',"import shortlist_replay as replay\nsizes,windows,_=replay.prepare_data()\nreplay.init_worker(sizes,windows)\ncase=next(c for c in replay.cases() if c['experiment']=='primary' and c['window_index']==3 and c['amplification']==10 and c['demand_target_quote']==1000000 and c['tolerance_median_bps']==100 and c['seed']==101)\nr=replay.run_case(case)\nassert abs(r['net_trading_quote']-r['endpoint_ledger_net_trading_quote'])<1e-4\nprint('Recomputed case:',r['window'],'A=',r['amplification'],'net trading=',r['net_trading_quote'],'fills=',r['fills'])"),
        ('short-limits','markdown','### Limits\nThese are five independent, centered one-hour episodes, not continuous January performance. Demand is a scenario model, not observed Dusk order flow. No annualization, unhedged LP-return claim, recentering or automatic concentration policy. Cells executed sequentially in Python, not certified in a Jupyter kernel.')]
    namespace={'__name__':'__main__'};count=max(c.get('execution_count') or 0 for c in nb['cells'])
    for identity,kind,body in cells:
        cell={'id':identity,'cell_type':kind,'metadata':{},'source':body.splitlines(keepends=True)}
        if kind=='code':
            stream=io.StringIO()
            with contextlib.redirect_stdout(stream):exec(compile(body,identity,'exec'),namespace)
            count+=1;cell.update(execution_count=count,outputs=[{'output_type':'stream','name':'stdout','text':stream.getvalue().splitlines(keepends=True)}])
        nb['cells'].append(cell)
    path.write_text(json.dumps(nb,indent=2)+'\n')


if __name__=='__main__':
    payload=build();notebook()
    print('Updated report:',len(payload['manifest']['blocks']),'blocks;',len(payload['manifest']['charts']),'charts;',len(payload['manifest']['tables']),'tables.')
