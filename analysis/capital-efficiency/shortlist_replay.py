"""Historical-price shortlist replay with explicitly modeled customer demand.

SOL/USDT January 2025 prices are observed. Customer intentions are synthetic:
Poisson arrivals, bootstrap sizes from prior-day CEX aggregated-trade clips,
independent fair buy/sell directions, and heterogeneous lognormal cost budgets.
No claim is made that CEX clips identify Dusk customers or organic flow.
"""
from concurrent.futures import ProcessPoolExecutor
from dataclasses import asdict
from datetime import datetime, timezone
from pathlib import Path
import argparse
import csv
import hashlib
import io
import json
import math
import random
import statistics
import time
import zipfile

from concentration_sweep import make_market, FIXED_FEES, V1_FEES, matched_capital, execution_cost
from fee_optimizer import optimal_trade as regime_optimal_trade

ROOT=Path(__file__).resolve().parent
DATA=ROOT/'market-data'
AMPS=[1,3,5,10]
SEEDS=[101,102,103,104,105]
CAPITAL=1_000_000.0
_SIZES=[]
_WINDOWS=[]


def read_zip(name):
    archive=zipfile.ZipFile(DATA/name)
    members=archive.namelist()
    assert len(members)==1 and members[0].endswith('.csv')
    with archive.open(members[0]) as handle:
        yield from csv.reader(io.TextIOWrapper(handle))


def quantile(values,q):
    values=sorted(values)
    return values[int(q*(len(values)-1))]


def utc(microseconds):
    return datetime.fromtimestamp(microseconds/1e6,timezone.utc).isoformat()


def read_klines(name,step_us):
    rows=[]
    previous=None
    for raw in read_zip(name):
        assert len(raw)==12 and all(x!='' for x in raw)
        t=int(raw[0]); op,hi,lo,cl=map(float,raw[1:5])
        vol,quote=float(raw[5]),float(raw[7])
        assert all(math.isfinite(v) for v in [op,hi,lo,cl,vol,quote])
        assert 0<lo<=op<=hi and lo<=cl<=hi
        assert vol>=0 and quote>=0 and int(raw[8])>=0
        assert 0<=float(raw[9])<=vol+1e-8 and 0<=float(raw[10])<=quote+1e-6
        assert int(raw[6])==t+step_us-1
        if previous is not None: assert t-previous==step_us
        previous=t
        rows.append({'t':t,'open':op,'high':hi,'low':lo,'close':cl,'base_volume':vol,'quote_volume':quote})
    return rows


def prepare_data():
    manifest=json.loads((DATA/'manifest.json').read_text())
    for source in manifest['sources']:
        actual=hashlib.sha256((DATA/source['file']).read_bytes()).hexdigest()
        assert actual==source['sha256']==(DATA/(source['file']+'.CHECKSUM')).read_text().split()[0]
    minutes=read_klines('SOLUSDT-1m-2025-01.zip',60_000_000)
    assert len(minutes)==31*1440
    assert utc(minutes[0]['t'])=='2025-01-01T00:00:00+00:00'
    hours=[]
    for i in range(24*60,len(minutes),60):
        rows=minutes[i:i+60]
        prices=[rows[0]['open']]+[r['close'] for r in rows]
        rv=sum(math.log(b/a)**2 for a,b in zip(prices,prices[1:]))
        hours.append({'start_us':rows[0]['t'],'start_utc':utc(rows[0]['t']),
                      'minute_realized_variance':rv,'return_fraction':prices[-1]/prices[0]-1,
                      'cex_volume_quote':sum(r['quote_volume'] for r in rows)})
    ranked=sorted(hours,key=lambda h:(h['minute_realized_variance'],h['start_us']))
    selections=[('quiet_p10',ranked[int(.1*(len(ranked)-1))]),
                ('median_p50',ranked[int(.5*(len(ranked)-1))]),
                ('volatile_p90',ranked[int(.9*(len(ranked)-1))]),
                ('extreme_p99',ranked[int(.99*(len(ranked)-1))]),
                ('largest_hourly_move',max(hours,key=lambda h:abs(h['return_fraction'])))]
    days={}
    windows=[]
    byminute={r['t']:r for r in minutes}
    for index,(label,hour) in enumerate(selections):
        day=hour['start_utc'][:10]
        if day not in days:
            rows=read_klines(f'SOLUSDT-1s-{day}.zip',1_000_000)
            assert len(rows)==86400
            days[day]=rows
        rows=[r for r in days[day] if hour['start_us']<=r['t']<hour['start_us']+3_600_000_000]
        assert len(rows)==3600
        for i in range(0,3600,60):
            minute=byminute[rows[i]['t']]
            assert abs(rows[i+59]['close']-minute['close'])<1e-8
            assert abs(sum(r['quote_volume'] for r in rows[i:i+60])-minute['quote_volume'])<.02
        initial=rows[0]['open']
        prices=[r['close']/initial for r in rows]
        windows.append({'index':index,'window':label,**hour,'initial_sol_usdt':initial,
            'second_realized_variance':sum(math.log(b/a)**2 for a,b in zip([1]+prices[:-1],prices)),
            'max_abs_one_second_move':max(abs(b/a-1) for a,b in zip([1]+prices[:-1],prices)),
            'prices':prices})
    sizes=[]
    prev_id=prev_time=None
    buy_count=0
    first_time=last_time=None
    for raw in read_zip('SOLUSDT-aggTrades-2025-01-01.zip'):
        assert len(raw)==8 and all(x!='' for x in raw)
        identity,t=int(raw[0]),int(raw[5])
        price,qty=float(raw[1]),float(raw[2])
        assert price>0 and qty>0 and math.isfinite(price*qty)
        assert raw[6].lower() in ['true','false'] and raw[7].lower() in ['true','false']
        assert int(raw[3])<=int(raw[4])
        if prev_id is not None:
            assert identity==prev_id+1 and t>=prev_time
        prev_id,prev_time=identity,t
        if first_time is None: first_time=t
        last_time=t
        assert utc(t).startswith('2025-01-01')
        sizes.append(price*qty)
        buy_count+=raw[6].lower()=='false'
    assert last_time<min(w['start_us'] for w in windows)
    calibration={'agg_trade_clips':len(sizes),'first_utc':utc(first_time),'last_utc':utc(last_time),
        'mean_clip_quote':statistics.fmean(sizes),'min_clip_quote':min(sizes),'max_clip_quote':max(sizes),
        **{f'p{int(q*100)}_clip_quote':quantile(sizes,q) for q in [.1,.5,.9,.95,.99]},
        'observed_aggressor_buy_fraction':buy_count/len(sizes),
        'sampling':'Full empirical notional distribution, no trimming; synthetic direction is independently 50/50, not inferred organic classification'}
    quality={'status':'usable as historical price and clip-size proxies, not observed Dusk demand',
        'minute_rows':len(minutes),'second_rows_checked':sum(len(r) for r in days.values()),
        'hourly_selection_population':len(hours),'selected_hours':len(windows),
        'checks':['All downloaded archive SHA256 values match publisher checksums',
            'Correct field counts, non-null fields, finite positive prices, nonnegative volumes and counts',
            'Unique continuous microsecond kline time keys; correct close timestamps and OHLC bounds',
            'Each second-data day has 86400 rows; monthly minutes have 44640 rows',
            'All 300 selected minute closes and quote volumes reconcile against second data',
            'Training aggregate trade IDs consecutive, timestamps nondecreasing, dates before replay windows',
            'Window selection uses external price statistics before any LP results are computed'],
        'limitations':['Single CEX last-trade reference can contain spread bounce and is not a consolidated efficient mid',
            'Observed CEX trade clips do not identify customer intent, DEX routing or non-toxic flow',
            'Selected percentiles and largest move are stress coverage, not occurrence-weighted performance',
            'January 2025 is an explicit historical sample, not recent trading performance'],
        'calibration':calibration,'windows':[{k:v for k,v in w.items() if k!='prices'} for w in windows]}
    (ROOT/'shortlist_data_quality.json').write_text(json.dumps(quality,indent=2)+'\n')
    (ROOT/'shortlist_market_inputs.json').write_text(json.dumps({'windows':windows,'calibration':calibration},indent=2)+'\n')
    return sizes,windows,quality


def init_worker(sizes,windows):
    global _SIZES,_WINDOWS
    _SIZES=sizes;_WINDOWS=windows


def intentions(window_index,seed,target,median_budget):
    if target==0: return {}
    rng=random.Random(seed+100000*window_index)
    rate=target/statistics.fmean(_SIZES)/3600
    elapsed=0.0
    result={}
    while True:
        elapsed+=rng.expovariate(rate)
        if elapsed>3600: break
        size=_SIZES[rng.randrange(len(_SIZES))]
        buy=rng.random()<.5
        budget=rng.lognormvariate(math.log(median_budget),.5)
        # Intentions execute after the completed second's reference update.
        second=max(1,math.ceil(elapsed))
        result.setdefault(second,[]).append((size,buy,budget))
    return result


def no_profitable_arb(start,external,acc,fees):
    base=fees.base_bps/10000
    pressure=fees.volatility_coefficient*acc
    vol=min((1-base)*pressure/(1+pressure),fees.volatility_cap_bps/10000,.5-base)
    # Even zero distance fee and zero impact cannot clear this minimum toll.
    return max(start/external,external/start)*(1-base-vol)<=1+1e-13


def run_case(case,grid_size=None):
    window=_WINDOWS[case['window_index']]
    amp=case['amplification'];capital=case['capital_quote']
    market=make_market(amp,capital)
    fees=V1_FEES if amp==1 else FIXED_FEES
    orders=intentions(case['window_index'],case['seed'],case['demand_target_quote'],case['tolerance_median_bps'])
    price,acc,external_before=1.0,0.0,1.0
    base_position,quote_position=market.point(price)
    inventory_beta=0.0
    sum_edge=sum_fees=customer_edge=customer_fees=arb_loss=0.0
    offered_volume=filled_volume=arb_volume=0.0
    fills=rejected=arb_count=intent_count=0
    vol_fees=distance_fees=base_fees=0.0
    decay_factor=2**(-1/fees.volatility_half_life_seconds)
    min_trading_pnl=0.0

    def execute(trade,is_customer):
        nonlocal price,acc,base_position,quote_position,sum_edge,sum_fees
        nonlocal customer_edge,customer_fees,arb_loss,filled_volume,arb_volume,fills,arb_count
        nonlocal vol_fees,distance_fees,base_fees,min_trading_pnl
        price,acc=trade['end_price'],trade['post_accumulator']
        base_position,quote_position=market.point(price)
        edge=-trade['curve_shortfall_usd'];lp_fee=trade['lp_fee_distributed_usd']
        sum_edge+=edge;sum_fees+=lp_fee
        value=1 if trade['buy'] else trade['external_price']
        vol_fees+=trade['volatility_fee']*value
        distance_fees+=trade['distance_fee']*value
        base_fees+=trade['base_fee']*value*(1-fees.protocol_base_share)
        volume=trade['gross_input']*value
        if is_customer:
            customer_edge+=edge;customer_fees+=lp_fee;filled_volume+=volume;fills+=1
        else:
            arb_loss-=edge+lp_fee;arb_volume+=volume;arb_count+=1
        min_trading_pnl=min(min_trading_pnl,sum_edge+sum_fees)

    def settle(external):
        for _ in range(40):
            if no_profitable_arb(price,external,acc,fees): return
            trade=(regime_optimal_trade(market,price,external,acc,fees) if grid_size is None else
                   market.optimal_trade(price,external,acc,fees,grid_size=grid_size))
            if trade['arb_profit_usd']<=1e-6: return
            execute(trade,False)
        raise AssertionError('Greedy arbitrage did not settle')

    for second,external in enumerate(window['prices'],1):
        acc*=decay_factor
        inventory_beta+=base_position*(external-external_before)
        external_before=external
        arb_available=second%case['arb_interval_seconds']==0
        if arb_available: settle(external)
        for notional,buy,budget in orders.get(second,[]):
            intent_count+=1;offered_volume+=notional
            gross=notional/(1 if buy else external)
            trade=market.trade(gross,price,external,acc,fees,buy)
            if execution_cost(trade,gross,external,buy)<=budget:
                execute(trade,True)
                if arb_available: settle(external)
            else: rejected+=1
    net=sum_edge+sum_fees
    final_nav=base_position*external_before+quote_position
    ledger=final_nav-capital-inventory_beta+sum_fees
    assert abs(net-ledger)<1e-4
    assert abs(net-(customer_edge+customer_fees-arb_loss))<1e-5
    assert abs(sum_fees-(vol_fees+distance_fees+base_fees))<1e-5
    assert fills+rejected==intent_count
    return {**case,'window':window['window'],'start_utc':window['start_utc'],
        'net_trading_quote':net,'net_trading_bps':net/capital*10000,
        'lp_fees_quote':sum_fees,'pre_fee_execution_edge_quote':sum_edge,
        'arb_loss_after_fees_quote':arb_loss,'customer_net_trading_quote':customer_edge+customer_fees,
        'base_lp_fees_quote':base_fees,'volatility_fees_quote':vol_fees,'outward_fees_quote':distance_fees,
        'offered_volume_quote':offered_volume,'filled_volume_quote':filled_volume,
        'fill_volume_fraction':filled_volume/offered_volume if offered_volume else None,
        'intents':intent_count,'fills':fills,'rejections':rejected,'arb_trades':arb_count,'arb_volume_quote':arb_volume,
        'lowest_cumulative_trading_pnl_quote':min_trading_pnl,'final_pool_price':price,
        'final_reference_price':external_before,'final_accumulator':acc,
        'inventory_price_change_pnl_quote':inventory_beta,'endpoint_ledger_net_trading_quote':ledger}


def cases():
    rows=[]
    for w in range(5):
        for amp in AMPS:
            common={'window_index':w,'amplification':amp,'capital_quote':CAPITAL,'capital_mode':'equal_capital',
                    'arb_interval_seconds':1,'experiment':'primary'}
            rows.append({**common,'demand_target_quote':0,'tolerance_median_bps':0,'seed':0})
            for target in [100000,1000000]:
                for tolerance in [20,50,100,200]:
                    for seed in SEEDS:
                        rows.append({**common,'demand_target_quote':target,'tolerance_median_bps':tolerance,'seed':seed})
            for interval in [5,30]:
                for seed in SEEDS[:3]:
                    rows.append({**common,'experiment':'latency','arb_interval_seconds':interval,
                                 'demand_target_quote':1000000,'tolerance_median_bps':100,'seed':seed})
    target_cost=execution_cost(make_market(1).trade(10000,1,1,0,V1_FEES),10000)
    for amp in [3,5,10]:
        capital=matched_capital(amp,10000,0,target_cost)
        for w in range(5):
            for seed in SEEDS:
                rows.append({'window_index':w,'amplification':amp,'capital_quote':capital,
                    'capital_mode':'matched_initial_10k','arb_interval_seconds':1,'experiment':'matched_capital',
                    'demand_target_quote':1000000,'tolerance_median_bps':100,'seed':seed})
    return rows


def summarize(rows):
    groups={}
    for r in rows:
        key=tuple(r[k] for k in ['experiment','window_index','amplification','capital_mode','arb_interval_seconds','demand_target_quote','tolerance_median_bps'])
        groups.setdefault(key,[]).append(r)
    summaries=[]
    for values in groups.values():
        r=values[0]
        summary={k:r[k] for k in ['experiment','window_index','window','start_utc','amplification','capital_quote','capital_mode','arb_interval_seconds','demand_target_quote','tolerance_median_bps']}
        for field in ['net_trading_quote','net_trading_bps','lp_fees_quote','pre_fee_execution_edge_quote',
                      'arb_loss_after_fees_quote','customer_net_trading_quote','offered_volume_quote','filled_volume_quote','fills','intents']:
            summary['mean_'+field]=statistics.fmean(x[field] for x in values)
        summary.update(seeds=len(values),min_net_trading_quote=min(x['net_trading_quote'] for x in values),
                       max_net_trading_quote=max(x['net_trading_quote'] for x in values))
        if r['amplification']!=1:
            paired=[]
            for value in values:
                baseline=next(b for b in rows if b['amplification']==1 and b['window_index']==value['window_index'] and
                    b['arb_interval_seconds']==value['arb_interval_seconds'] and b['seed']==value['seed'] and
                    b['demand_target_quote']==value['demand_target_quote'] and b['tolerance_median_bps']==value['tolerance_median_bps'])
                paired.append((value['net_trading_quote']-baseline['net_trading_quote'],value['net_trading_bps']-baseline['net_trading_bps']))
            summary.update(mean_delta_v1_quote=statistics.fmean(x[0] for x in paired),
                           min_delta_v1_quote=min(x[0] for x in paired),max_delta_v1_quote=max(x[0] for x in paired),
                           mean_delta_v1_bps=statistics.fmean(x[1] for x in paired),seed_wins_vs_v1=sum(x[0]>0 for x in paired))
        else:
            summary.update(mean_delta_v1_quote=0,min_delta_v1_quote=0,max_delta_v1_quote=0,mean_delta_v1_bps=0,seed_wins_vs_v1=0)
        summaries.append(summary)
    return summaries


def validate(rows):
    checks=['Every replay reconciles fee components, customer/arb P&L, and an independent endpoint inventory ledger',
            'Identical generated intentions and cost budgets are offered to all concentrations for each paired case',
            'Training clip sizes precede every replay window; intent directions and budgets do not use future returns']
    bykey={}
    for r in rows:
        key=tuple(r[k] for k in ['window_index','seed','demand_target_quote','tolerance_median_bps'])
        common=(r['offered_volume_quote'],r['intents'])
        if key in bykey: assert common==bykey[key]
        bykey[key]=common
    rng=random.Random(884)
    checked=0
    for _ in range(100):
        a=rng.choice(AMPS);fees=V1_FEES if a==1 else FIXED_FEES
        start=rng.uniform(.92,1.08);external=start*math.exp(rng.uniform(-.012,.012));acc=rng.uniform(0,.02)
        if no_profitable_arb(start,external,acc,fees):
            t=make_market(a).optimal_trade(start,external,acc,fees)
            assert t['arb_profit_usd']<=1e-6
            checked+=1
    checks.append(f'Minimum-toll screening agrees with unscreened optimizer in {checked} sampled no-arbitrage states')
    for amp in AMPS:
        row=next(r for r in rows if r['experiment']=='primary' and r['window']=='extreme_p99' and r['amplification']==amp and r['demand_target_quote']==1000000 and r['tolerance_median_bps']==100 and r['seed']==101)
        refined=run_case(row,grid_size=1600)
        assert abs(refined['net_trading_quote']-row['net_trading_quote'])<.05
        assert refined['fills']==row['fills']
    checks.append('Four extreme-volatility replays agree within 0.05 quote units and identical fills against independent 1600-point grid bracketing')
    for _ in range(60):
        amp=rng.choice(AMPS);market=make_market(amp);fees=V1_FEES if amp==1 else FIXED_FEES
        start=rng.uniform(.9,1.1);external=start*math.exp(rng.uniform(-.08,.08));acc=rng.uniform(0,.02)
        solved=regime_optimal_trade(market,start,external,acc,fees)
        dense=market.optimal_trade(start,external,acc,fees,grid_size=1600)
        assert abs(solved['arb_profit_usd']-dense['arb_profit_usd'])<1e-5
    checks.append('Sixty randomized off-center buy/sell opportunities: two-regime solver agrees with independent dense search')
    return checks


def save_csv(name,rows):
    with (ROOT/name).open('w',newline='') as f:
        writer=csv.DictWriter(f,fieldnames=rows[0].keys());writer.writeheader();writer.writerows(rows)


def main(workers=4):
    started=time.monotonic()
    sizes,windows,quality=prepare_data()
    print('Data checks passed:',quality['calibration'],flush=True)
    init_worker(sizes,windows)
    jobs=cases();rows=[]
    with ProcessPoolExecutor(max_workers=workers,initializer=init_worker,initargs=(sizes,windows)) as pool:
        for i,result in enumerate(pool.map(run_case,jobs,chunksize=4),1):
            rows.append(result)
            if i%100==0: print('Replays',i,'/',len(jobs),'elapsed',round(time.monotonic()-started,1),'seconds',flush=True)
    # Save the expensive replay output before a validation failure, but never
    # publish results or summaries until all checks pass.
    (ROOT/'shortlist_replay_checkpoint.json').write_text(json.dumps(rows)+'\n')
    checks=validate(rows)
    summaries=summarize(rows)
    save_csv('shortlist_replays.csv',rows);save_csv('shortlist_summary.csv',summaries)
    result={'parameters':{'amplifications':AMPS,'fees':asdict(FIXED_FEES),'capital_quote':CAPITAL,
        'demand_targets_quote_per_hour':[0,100000,1000000],'tolerance_medians_bps':[20,50,100,200],
        'tolerance_log_sigma':.5,'seeds':SEEDS,'arb_intervals_seconds':[1,5,30],
        'direction':'independent fair buy/sell','arrivals':'Poisson, rate=target volume / empirical mean clip notional',
        'fee_destination':'distributed; no compounding','curve':'fixed center and fixed amplification; 100/400bps widths',
        'unit':'USDT quote units; no USD peg conversion is performed'},'rows':rows,'summary':summaries}
    (ROOT/'shortlist_results.json').write_text(json.dumps(result,indent=2)+'\n')
    validation={'status':'share with caveats','generated_at':datetime.now(timezone.utc).isoformat(),
        'checks':checks,'replays':len(rows),'summary_rows':len(summaries),'elapsed_seconds':time.monotonic()-started,
        'data_quality_file':'shortlist_data_quality.json','source_manifest':'market-data/manifest.json',
        'method_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        'source_hashes':{name:hashlib.sha256((ROOT/name).read_bytes()).hexdigest() for name in
                         ['shortlist_replay.py','fee_optimizer.py','concentration_sweep.py','dynamic_fees.py','model.py']},
        'limitations':['Historical reference prices with synthetic demand, not observed Dusk performance',
            'CEX aggregate clips supply only a size proxy; demand capture and price tolerance remain assumptions',
            'Five selected hours and five demand seeds do not estimate regime frequencies or real-world confidence intervals',
            'One-second last-trade reference; subsecond movements, cross-venue basis and strategic MEV not modeled',
            'Fixed center, fixed concentration, no active hLP/debt/retention/compounding or integer program parity']}
    (ROOT/'shortlist_validation.json').write_text(json.dumps(validation,indent=2)+'\n')
    print('Complete',len(rows),'replays;',len(checks),'new checks; elapsed',round(time.monotonic()-started,1),'seconds',flush=True)
    for w in range(5):
        subset=[r for r in summaries if r['experiment']=='primary' and r['window_index']==w and r['demand_target_quote']==1000000 and r['tolerance_median_bps']==100]
        print(w,[(r['amplification'],round(r['mean_net_trading_quote'],2),round(r['mean_filled_volume_quote'])) for r in subset],flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--workers',type=int,default=4)
    main(parser.parse_args().workers)
