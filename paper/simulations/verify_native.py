#!/usr/bin/env python3
"""Check retained native evidence and the paper's numeric examples; no program execution."""
import json
import math
from pathlib import Path

HERE = Path(__file__).resolve().parent

def close(a, b, tolerance=2e-6):
    assert math.isclose(a, b, rel_tol=0, abs_tol=tolerance), (a, b)

def main():
    data = json.loads((HERE / 'native-hlp-flow.json').read_text())
    cases = data['cases']
    assert len(cases) == 8
    for c in cases:
        before, after = c['before'], c['after']
        p = after['price']
        holders = {h['name']: h for h in after['holders']}
        close(sum(h['net_usd'] for h in holders.values()), after['cash_base'] * p + after['cash_quote'])
        if c['with_hlp']:
            close(holders['base_hlp']['quote'], holders['base_hlp']['debt_quote'])
            close(holders['quote_hlp']['base'], holders['quote_hlp']['debt_base'])
        fee_value = c['fee_atoms']['claimable_fee_debit'] / 1e9 * p
        cash_delta = (after['cash_base'] - before['cash_base']) * p + after['cash_quote'] - before['cash_quote']
        close(cash_delta + c['output'] - c['input'] * p + fee_value, 0)
    for amp in (1, 4):
        pair = [c for c in cases if c['amp'] == amp and c['swap_fee_bps'] == 0]
        assert len(pair) == 2
        close(pair[0]['output'], pair[1]['output'])
        for asset in ('base', 'quote'):
            close(pair[0]['after']['holders'][0][asset], pair[1]['after']['holders'][0][asset])
    c = next(c for c in cases if c['amp'] == 4 and c['with_hlp'] and c['swap_fee_bps'] == 0)
    close(c['output'], 19.360948321)
    close(c['after']['price'], .711853017)
    close(c['trader_gain_at_final_price'], 3.5419923878)
    # The $2 shifted-product example is separate from the $1 static CPMM gap.
    x = 40000 / 180 - 100
    p = (80 + 100) / (x + 100)
    close(p, .81)
    close((100*p+100) - (x*p+80), 2)
    close(100*.81 - 100*(2*math.sqrt(.81)-1), 1)
    for weight, leverage in ((.5,2),(.625,1.6),(.4,2.5)):
        close(1/weight, leverage)
    print(json.dumps({'status':'passed','native_cases_checked':len(cases),'source_commit':data['source_commit'],'checks':['holder net claims equal marked cash','opposite claim/debt match','cash + trader + fee conservation','matched ordinary inventory with/without hLP','published native values','shifted-product loss','static CPMM gap','inventory-weight leverage']}))

if __name__ == '__main__':
    main()
