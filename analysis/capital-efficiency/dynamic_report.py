"""Extend the full original artifact with dynamic-fee evidence."""
from pathlib import Path
import json
import sqlite3

from dynamic_fees import experiments, validate


def extend_artifact(payload, root):
    data = experiments()
    checks = validate()
    shocks = [r for r in data['dynamic_shocks'] if r['external_jump'] == 0.01]
    execution = {r['profile']: r for r in data['dynamic_execution']}
    values = []
    for order, row in enumerate(shocks):
        ex = execution[row['profile']]
        values.append((order, row['profile'], row['curve_shortfall_usd'], row['base_fee'],
                       row['distance_fee'], row['volatility_fee'], ex['output'],
                       ex['capital_for_v1_execution_usd']))
    def literal(value):
        return "'" + value.replace("'", "''") + "'" if isinstance(value, str) else repr(value)
    query = """-- Actual independent metric calculation from dynamic_fees.py curve/fee outputs.
-- $1m capital and +1% external jump for LP shortfalls; gross $10k buy for execution.
-- Source curve, fee ordering, optimizer and parameter/state assumptions remain
-- in dynamic_fees.py and dynamic_results.json; inputs below are synthetic.
WITH modeled(ordering, profile, curve_shortfall, base_fee, distance_fee,
             volatility_fee, ten_k_buy_output, matched_execution_capital) AS (
 VALUES\n""" + ",\n".join(" (" + ", ".join(literal(v) for v in row) + ")" for row in values) + """
)
SELECT ordering, profile,
       curve_shortfall - (0.8*base_fee + distance_fee + volatility_fee) AS lp_loss_distributed_usd,
       curve_shortfall - 0.8*base_fee AS lp_loss_retained_usd,
       distance_fee + volatility_fee AS protected_bucket_if_retained_usd,
       10000*(10000/ten_k_buy_output-1) AS average_cost_bps,
       matched_execution_capital,
       1-matched_execution_capital/1000000 AS capital_saved_same_execution
FROM modeled ORDER BY ordering;
"""
    (root / 'dynamic_verification.sql').write_text(query)
    connection = sqlite3.connect(':memory:')
    connection.row_factory = sqlite3.Row
    rows = [dict(r) for r in connection.execute(query)]
    for row, original in zip(rows, shocks):
        assert abs(row['lp_loss_distributed_usd']-original['lp_loss_distributed_usd']) < 1e-7
        assert abs(row['lp_loss_retained_usd']-original['lp_loss_retained_usd']) < 1e-7
    source = {"id": "dynamic", "label": "Dynamic-fee model and independent metric calculation",
              "path": "dynamic_verification.sql", "query": {"engine": "SQLite", "language": "sql", "sql": query,
              "description": "Synthetic conditional states using current Dusk input-denominated gross-path fees. Original computation: dynamic_fees.py; complete rows: dynamic_results.json. Source locations: AMM mod.rs 913-1033 and 1440-1535; fees.rs 395-484 and 594-626; ledger.rs 310-370.",
              "tables_used": ["modeled (inline outputs from dynamic_fees.py)", "dynamic_results.json", "programs/dusk/src/transitions/amm/mod.rs", "programs/dusk/src/transitions/amm/fees.rs", "programs/dusk/src/transitions/ledger.rs"],
              "filters": ["$1m balanced starting capital; center=spot=1; 5x peak, 100/400-bps core/fade",
                          "base=30bps; distance coefficient=10; volatility coefficient=1; each surcharge cap=200bps",
                          "volatility half-life=60s; shock cap=0.05; accumulator cap=0.10; initial accumulator=0/0.005/0.01",
                          "protocol takes 20% of base fee only; distributed surcharge is entirely LP-owned",
                          "input-denominated fees; no hLP, public debt, compounding, launch premiums, transfer fees, or center move",
                          "Greedy same-slot arbitrage is a robustness experiment, not a globally optimized multi-transaction strategy"],
              "metric_definitions": ["LP loss distributed = marked curve shortfall - 80% of base fee - all dynamic fees",
                                    "LP loss retained = marked curve shortfall - 80% of base fee; protected bucket excluded",
                                    "Execution cost = 10000*(gross quote/base output/starting price-1)",
                                    "Same-execution capital saving = 1 - required capital / $1m; match v1's $10k buy at 230.09bps",
                                    "Warm accumulator=0.005 is a conditional internal signal, not 0.5% annualized volatility"]}}
    payload['manifest']['sources'].append(source)
    if payload['sources'] is not payload['manifest']['sources']:
        payload['sources'].append(source)
    payload['snapshot']['datasets']['dynamic_comparison'] = rows
    payload['snapshot']['datasets']['dynamic_loss_chart'] = [
        {**row, 'fee_destination': destination, 'net_lp_shortfall_usd': row[field]}
        for row in rows for destination, field in [('Surcharge distributed', 'lp_loss_distributed_usd'), ('Surcharge retained', 'lp_loss_retained_usd')]]
    for key in ['dynamic_cooldown', 'dynamic_coefficients', 'dynamic_bursts']:
        payload['snapshot']['datasets'][key] = data[key]
    blocks = payload['manifest']['blocks']
    summary = next(b for b in blocks if b['id'] == 'summary')
    summary['body'] = """## Executive Summary
- **Dusk concentration improves local execution depth while raising gross LVR exposure.** The 5× example has five times the centered depth and five times the local gross LVR per dollar; dynamic fees determine how much adverse-selection cost remains with LPs.
- **Dynamic fees can reverse the net comparison in an already volatile state.** In the conditional +1% shock experiment, 5× Dusk loses $3.03 when surcharges are distributed, versus v1’s $7.12. From quiet conditions it loses $21.20. These are modeled event losses, not measured daily LVR or returns.
- **Recenter retention changes who benefits immediately.** The same volatile-state trade leaves ordinary LPs with $12.35 of loss when its $9.32 surcharge is retained. The protected bucket is not immediately redeemable yLP value.
- **After dynamic fees, finite-size capital savings are smaller than the peak multiplier implies.** Matching v1’s $10k execution requires about $624k in the quiet profile and $738k in the already volatile profile: 38% and 26% less than $1m. Market-wide profitability remains unmeasured."""
    new_blocks = [
        {"id": "dynamic-definition", "type": "markdown", "sourceId": "dynamic", "body": """## Dynamic protection depends on the state before the trade
This extension uses the current input-denominated base, volatility and gross-path distance fee formulas in real-valued arithmetic. The illustrated controls are base fee 30 bps, distance coefficient 10, volatility coefficient 1, caps of 200 bps for each surcharge, and a 60-second volatility half-life. Coefficients are decimal multiples of NAD in the program. They are test inputs, not an inferred market configuration.

Every shock starts at center price 1 with $1m ordinary capital. Quiet, warm and hot mean pre-trade accumulators of 0, 0.005 and 0.01. These are conditional internal states, not annualized volatility measurements. The arbitrageur chooses its most profitable trade size; the fee is not imposed on a fixed v1-sized trade.

Volatility pressure is coefficient × decayed accumulator. The volatility fee is (gross−base fee) × pressure/(1+pressure), bounded by its gross-input share cap. Distance is charged from the potential F(u)=4×coefficient×u³/[3×center_input×(center_input+u)], where u is positive outward input-reserve displacement. Evaluate its difference over the post-base/post-volatility provisional input path, cap it, then execute the remaining input. The live gross-path calculation does not use the legacy test-only Huber solver."""},
        {"id": "dynamic-loss", "type": "markdown", "sourceId": "dynamic", "body": """## Distributed dynamic fees can outweigh the extra exposure in a small shock
The chart compares net ordinary-LP shortfalls after the same +1% external-price jump, with initial capital held equal. Lower is better. Base-only 5× Dusk loses $35.58; adding distance and volatility fees reduces this to $21.20 from quiet conditions and $3.03 with a warm accumulator. The warm case is about 57% below v1’s $7.12.

The fee-routing comparison is crucial: the warm trade pays $9.32 of dynamic fees. If those fees are retained, immediate ordinary-LP loss is $12.35, about 74% above v1. This is a transfer into protected recenter funding, not disappearance of economic value or a claim about ultimate recenter outcomes. The protocol share is taken only from base fees; all non-retained surcharge belongs to yLP.

At accumulator 0.01, even the small-trade base-plus-volatility toll exceeds the +1% opportunity, so the model executes no immediate arbitrage. Zero immediate loss also means a stale quote; later fee decay and price changes remain relevant."""},
        {"id": "dynamic-loss-chart", "type": "chart", "chartId": "dynamic-loss"},
        {"id": "dynamic-capital", "type": "markdown", "sourceId": "dynamic", "body": """## Paying for protection uses part of the execution advantage
The table pairs the shock comparison with a separate $10,000 organic buy from the same centered state. At $1m capital, v1 costs 230.09 bps all-in. Dusk 5× costs 70.09 bps with base fees alone, 122.50 bps when quiet with distance fees, 172.39 bps when warm, and 222.28 bps when hot.

Resizing capital until that $10k buy matches v1’s 230.09-bps cost leaves savings of 73.9% with base fees only, 37.6% when quiet with dynamic fees, 26.2% when warm, and 5.1% when hot. This finite-trade comparison includes the extra fees and any band crossings. It differs from the 80% local peak-depth calculation.

These savings measure conditional execution capacity, not risk-adjusted return. The loss columns use equal $1m capital; the capital-saving column is a separate equal-execution experiment. Do not combine them as though both used the resized pool."""},
        {"id": "dynamic-table", "type": "table", "tableId": "dynamic"},
        {"id": "dynamic-robustness", "type": "markdown", "sourceId": "dynamic", "body": """## The apparent winner changes as the signal decays or the shock grows
With no intervening swaps and a 60-second half-life, the warm-state +1% shock loss rises from $3.03 immediately to $6.98 after 30 seconds and $10.50 after 60 seconds, assuming surcharge distribution. That crosses back above v1’s $7.12 benchmark. Each row is an independent experiment with decayed pre-state, not repeated losses along one price path.

A same-slot greedy-arbitrage burst also shows the limitation of a small-shock win. For a +5% external jump, warm 5× Dusk loses $365.97 with distribution, versus v1’s $275.90. The quiet Dusk burst takes two trades and loses $437.44. The larger move reaches fee limits and different inventory regions; the +1% result is not a uniform LVR reduction factor.

The experiments check the formulas, accounting and repeated greedy execution. They do not optimize a strategic multi-transaction attack or reproduce the integer program, center admission, cash constraints or hLP settlement. To estimate daily net LVR, feed real price paths and trade demand through those mechanisms and track volatility updates, fee decay and recenter funding over time."""},
    ]
    insertion = next(i for i,b in enumerate(blocks) if b['id'] == 'parameters')
    blocks[insertion:insertion] = new_blocks
    caveats = next(b for b in blocks if b['id'] == 'caveats')
    caveats['body'] = caveats['body'].replace('The model uses floating-point nested constant-product inventory and exogenous fixed fees.', 'The original benchmark uses floating-point nested constant-product inventory and fixed fees. The extension applies the current input-denominated dynamic-fee formulas to conditional one-shock and same-slot greedy-burst experiments.')
    caveats['body'] = caveats['body'].replace('exact dynamic fees,', 'integer-exact fee execution,')
    payload['manifest']['charts'].append({"id": "dynamic-loss", "title": "Net LP shortfall after a +1% external-price jump",
        "subtitle": "$1m initial capital; conditional fee states; USD per event", "showDescription": True,
        "type": "bar", "intent": "comparison", "question": "How do dynamic fees and fee routing affect ordinary LP loss?",
        "rationale": "Grouped bars compare five configurations and two ownership treatments on one dollar scale; category and legend labels distinguish groups.",
        "dataset": "dynamic_loss_chart", "sourceId": "dynamic", "layout": "full",
        "encodings": {"x": {"field": "profile", "type": "nominal"}, "y": {"field": "net_lp_shortfall_usd", "type": "quantitative"}, "color": {"field": "fee_destination", "type": "nominal"}},
        "xAxisTitle": "Fee profile", "yAxisTitle": "Net LP shortfall (USD)", "valueFormat": "number"})
    payload['manifest']['tables'].append({"id": "dynamic", "title": "Dynamic fee and capital comparison", "dataset": "dynamic_comparison",
        "sourceId": "dynamic", "defaultSort": {"field": "lp_loss_distributed_usd", "direction": "desc"}, "density": "spacious", "layout": "full",
        "columns": [{"field": "profile", "label": "Profile"}, {"field": "lp_loss_distributed_usd", "label": "LP loss: distributed ($)", "format": "number"},
                    {"field": "lp_loss_retained_usd", "label": "LP loss: retained ($)", "format": "number"},
                    {"field": "average_cost_bps", "label": "$10k cost (bps)", "format": "number"},
                    {"field": "capital_saved_same_execution", "label": "Capital saved", "format": "percent"}]})
    (root/'dynamic_results.json').write_text(json.dumps(data, indent=2)+'\n')
    (root/'dynamic_validation.json').write_text(json.dumps({'status': 'passed', 'checks': checks+['Independent SQLite recomputation of dynamic LP shortfalls']}, indent=2)+'\n')
    return payload
