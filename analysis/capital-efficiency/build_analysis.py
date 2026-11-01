"""Build the report payload and an executed, standard-library-only notebook."""
from pathlib import Path
from datetime import datetime, timezone
import contextlib
import hashlib
import io
import json
import subprocess
import sys
import sqlite3
import uuid

from model import save_results
from dynamic_report import extend_artifact

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[1]
V1 = REPO.parent / "omnipair-rs"
data = save_results(ROOT)
timestamp = datetime.now(timezone.utc).isoformat()
sources = [
    {"id": "model", "label": "Executed analytical scenarios — model.py and results.json",
     "path": "model.py", "query": {"engine": "Python standard library", "language": "python",
      "description": "Real-valued nested CPMM curve derived from the inspected Dusk implementation; deterministic synthetic scenarios, not market observations.",
      "executed_at": timestamp, "tables_used": ["scenarios.csv", "executions.csv", "shocks.csv", "feasibility.csv", "sensitivity.csv"],
      "filters": ["$1m initial ordinary reserve NAV; price=center=1", "No public debt, active hLP, reserve cash limits, recenter moves, or token transfer fees", "Annual volatility=100%; 365 days; local instantaneous rate expressed per day", "Fixed input fee=30 bps and LP fee share=80% are illustrative, not deployment values"],
      "metric_definitions": ["Local gross LVR/day = active liquidity * sqrt(price) * annual variance / (4*365)", "Average buy cost bps = 10000*(gross quote/base output/start price - 1)", "Same peak depth capital saving = 1-1/amplification; only local centered depth is matched", "Net LP jump shortfall = external-price value of output - net curve input - LP-owned fee", "Residual fractions in sensitivity.csv are assumptions; they are not measured fee effectiveness"]}},
    {"id": "dusk", "label": "Dusk current local curve, fee and accounting implementation",
     "href": "https://github.com/omnipair/dusk/tree/26bc75b217a96a6241b82b05cd52c1b587272dc1/programs/dusk",
     "query": {"description": "Local working tree inspected September 7, 2026. Source files and hashes recorded in source-notes.json. Uncommitted README changes take precedence over the linked commit for documentation.",
               "tables_used": ["programs/dusk/CONCENTRATION.md", "programs/dusk/src/transitions/amm/curve.rs", "programs/dusk/src/transitions/amm/fees.rs", "programs/dusk/src/transitions/amm/mod.rs", "programs/dusk/src/state/market.rs", "programs/dusk/README.md"]}},
    {"id": "v1", "label": "Omnipair v1 local swap implementation",
     "href": "https://github.com/omnipair/omnipair-rs/tree/84a8ef7a2a3a81e750435b86d7191bf43413d6a4/programs/omnipair",
     "query": {"description": "Local checkout baseline; not independently matched to a deployed binary.", "tables_used": ["programs/omnipair/src/instructions/spot/swap.rs", "programs/omnipair/src/utils/gamm_math.rs", "programs/omnipair/src/state/pair.rs"]}},
    {"id": "lvr", "label": "Milionis et al. — Automated Market Making and Loss-Versus-Rebalancing", "href": "https://arxiv.org/html/2208.06046v5"},
    {"id": "fees", "label": "Milionis et al. — Arbitrage Profits in the Presence of Fees", "href": "https://arxiv.org/abs/2305.14604"}
]

blocks = []
def paragraph(identity, body, source=None):
    block = {"id": identity, "type": "markdown", "body": body}
    if source:
        block["sourceId"] = source
    blocks.append(block)

title = "Dusk versus Omnipair v1: capital efficiency and toxic flow"
paragraph("title", f"# {title}")
paragraph("summary", """## Executive Summary
- **Concentration buys execution depth and increases local adverse-selection exposure.** A 5× centered configuration provides 5× marginal depth; it can match v1’s centered depth with 80% less ordinary reserve capital. This is not a claim of 80% less total system capital or equal execution across the full price range.
- **The same concentration multiplies gross local LVR per dollar.** Under the illustrative $1m, 100%-annual-volatility scenario, the fee-free local benchmark is $342/day for v1 and $1,712/day at 5×. These are instantaneous rates expressed per day, not forecasts or measured daily losses.
- **A net profitability advantage remains unmeasured.** Dusk’s dynamic fees, retained recenter funding, volume response and execution constraints need a common-path replay before claiming lower net toxic-flow losses or selecting an optimum.""")
paragraph("definitions", """## Compare equal capital and equal execution quality separately
**Capital** here is initial ordinary reserve NAV marked at an independent external price. The analytical baseline starts centered, balanced, fully cash-backed, without borrowing or hLP. For live comparisons, also report cash, lender claims, external borrower collateral and total external equity separately; eliminate internal funding claims when consolidating the system.

**Execution efficiency** means usable trade size for a specified all-in execution cost per dollar of capital. **LVR** means the cost of stale-price rebalancing against a frictionless strategy with the same inventory exposure; it is different from impermanent loss versus holding. **Toxic-flow loss** means adverse selection attributable to informed trading, measured with signed external-price markouts and explicit fee accounting.

Run both equal-capital and equal-trade-size/slippage experiments. At equal centered depth, the local dollar LVR benchmark is also equal, although Dusk uses less capital. That does not make the return per dollar safer.""")
paragraph("execution", """## Near the center, less capital can support the same trade
The chart compares a $10,000 quote-to-base buy against $1m initial capital, starting at the band center. Every case pays the same illustrative 30-bps input fee. The Dusk cases use a 100-bps core width and a 400-bps additional shoulder width.

**At 5×, average all-in cost falls from 230.09 bps to 70.09 bps.** Before fees, the values are 200 bps and 40 bps. The 2× case crosses the core boundary, so finite-trade savings do not equal its advertised peak multiplier. A dynamic surcharge must be included in the final routing comparison.""", "model")
blocks.append({"id": "execution-chart", "type": "chart", "chartId": "execution"})
paragraph("lvr-result", """## Gross LVR rises with the depth available to arbitrageurs
For a constant-product pool, gross local LVR divided by NAV is annual variance divided by eight per year. For Dusk’s active segment, differentiating its inventory curve gives local gross LVR = active liquidity × √price × variance / 4. At the centered point this becomes amplification × capital × variance / 8.

The chart expresses this local rate per day at 100% annual volatility. At 50% volatility every value is one quarter as large; at 150% it is 2.25× as large. Fees, discrete block timing, jumps and center changes require additional modeling. Do not read these bars as realized fee-paying arbitrage losses.""")
blocks.append({"id": "lvr-chart", "type": "chart", "chartId": "lvr"})
paragraph("regions", """## The 5× advantage shrinks outside the core
The table shows active depth relative to a v1 curve with the same initial capital. For the 5× example, depth is 5× in the core, 2.97× in either shoulder and 0.94× in either tail. Core price bounds are center/1.01 to center×1.01; outer bounds are center/1.05 to center×1.05. The lower bounds are reciprocal, not exactly −1% and −5%.

**Use volatility-weighted occupancy when summarizing LVR.** A simple percentage of clock time in the core misses whether the largest price moves happened while concentrated liquidity was active. Recenter changes also require their own inventory and funding ledger.""", "model")
blocks.append({"id": "regions-table", "type": "table", "tableId": "regions"})
paragraph("protection", """## Dynamic fees must compensate for the extra exposure
Let r be residual net adverse-selection cost divided by each configuration’s gross local LVR benchmark, after crediting fees from the same toxic trades to the selected LP owner. In the matched centered comparison, Dusk/v1 residual cost per dollar is approximately A × r_Dusk/r_v1.

**At 5×, r_Dusk must be below one fifth of r_v1 to reduce net toxic cost per dollar.** That is more than an 80% reduction in the residual factor relative to v1. At 2× the hurdle is more than 50%; at 10× it is more than 90%. These are break-even conditions, not estimates of achieved protection. Organic fee revenue can still make Dusk more profitable even when toxic cost is higher.

For a simple budget check, a 30-bps fee with 80% allocated to LPs yields 24 bps per dollar traded. Covering the unsuppressed local LVR benchmark requires daily turnover of 0.143× capital for v1 and 0.713× for the 5× case. Use this as a fee-income hurdle, not a forecast of required actual volume. In a residual-cost model, add organic fees only: toxic fees have already been credited in r.""")
paragraph("jump", """## Higher fees can stop a small arbitrage without proving lower total losses
For one external +1% jump from the center, a constant 30-bps fee and 80% LP allocation produce net LP shortfalls of $7.12 for v1 and $35.58 for 5× Dusk. The 5× trade generates $26.17 of fees, but only $20.94 belongs to the LP in this example; the arbitrageur retains $30.35.

An exogenous 100-bps fee makes this isolated +1% jump unprofitable to trade immediately. This merely leaves a stale quote: a later larger move, cheaper route or later fee decay changes the result. The calculation does not simulate Dusk’s dynamic schedule, and it does not establish that Dusk would charge 100 bps on the first jump.""", "model")
paragraph("code", """## Current code makes the fee and ownership assumptions decisive
**Concentration and dynamic fees are configurable.** The default AMM configuration is 1× CPMM with zero widths and zero dynamic-fee coefficients; the protocol name alone implies no concentration gain.

The volatility charge uses the decayed accumulator from preceding successful swaps. That accumulator sums capped absolute relative price movements, not statistical variance. A first shock after quiet conditions therefore cannot rely on its own volatility update for protection. The distance charge can apply to the current outward path, but distance from an internal center is not a direct observation of informed flow; a stale center can misclassify economically useful repricing.

While recenter retention is armed, dynamic surcharge goes into protected, non-quoteable reserves outside ordinary yLP NAV and withdrawal claims. Do not count it simultaneously as immediately distributable LP income and as free center-adjustment capital. hLP funding is also an internal transfer when evaluating consolidated participants; synthetic hLP leverage is not extra trader-visible depth. An active-hLP replay must separately measure yLP, each hLP and protocol outcomes, plus settlement rejections.""", "dusk")
paragraph("parameters", """## Parameterize five groups before choosing a configuration
1. **Curve:** test A = 1, 2, 5 and 10; core/fade widths of 100/400 and 500/1,500 bps. Enforce a positive full-range tail. Width and amplification are coupled: 10× with 1,000/4,000-bps widths is infeasible in this model, with a strict upper bound near 8.69×.
2. **Fee response and ownership:** vary base fee, distance coefficient/cap, volatility coefficient/cap/half-life, fee denomination, protocol allocation and recenter retention. Model organic demand response to all-in cost; do not assume higher fees leave volume unchanged.
3. **Center policy:** vary EMA half-life, movement threshold, step and minimum interval. Compare funded moves and deferred moves; measure the economic cost and time spent away from useful depth.
4. **External flow:** use annual volatility scenarios of 50%, 100%, 150% and 200%, plus jumps, trends, reversals, quiet-to-shock transitions, trade-size distributions and arbitrage latency. Retain the external reference-price path for markouts.
5. **Balance sheet:** vary cash utilization, borrow interest and bad debt, hLP participation/funding, liquidity withdrawals and execution rejection rates. External yield can improve LP economics but is not evidence of reduced LVR.""")
paragraph("replay", """## Next: replay identical market opportunities through both engines
**Start with 2× and 5× as candidates, not an optimum.** Compare v1, Dusk at 1×, concentrated Dusk without dynamic fees, and concentrated Dusk with dynamic fees. Add recentering and active hLP as separate ablations so any advantage has an identifiable cause.

Use the same independent external-price path and the same exogenous organic trade intentions. Let arbitrage size, successful fills and organic routing respond to each pool’s actual quotes. Replaying identical executed fills would conceal the fee-demand tradeoff.

Record signed base quantity q (positive when the trader buys base), pre-fee execution price P_exec and independent midprice P_h after 1 second, 10 seconds and 60 seconds. Trader markout is q×(P_h−P_exec); LP execution edge is its negative plus LP-owned fees. Keep each horizon separate. Do not subtract both this markout and LVR from the same P&L, or credit a fee twice.

Judge configurations on net LP edge per time-weighted NAV, organic volume captured, all-in execution cost at matched size, cash fill capacity, rejected demand and drawdown. Report cohort results for ordinary yLP, each hLP side and the consolidated system.""")
paragraph("questions", """## The remaining evidence is market-specific
The next calibration needs a market or pair, a v1 reference configuration/state, and timestamped independent prices plus trade-intention or trade data. Which widths and fees win depends on whether flow is mean-reverting, trending, informed or price-sensitive. No historical dataset was supplied or used here; the generic volatile-token/stablecoin setup is an explicit assumption.""")
paragraph("caveats", """## Scope and assumptions
This is a verified mathematical first pass against the inspected local source, with reproducible synthetic scenarios. Dusk is pre-deployment. The v1 checkout was not matched to a deployed binary, and current Dusk documentation contains pre-existing local edits.

The model uses floating-point nested constant-product inventory and exogenous fixed fees. It does not reproduce atom rounding, exact dynamic fees, block scheduling, cash constraints, hLP settlement, center admission, launch protections, lending or liquidations. Gross local LVR and the single-jump experiment answer different questions and must not be added together. A full replay remains necessary for measured net efficiency and an optimum parameter set.""")

datasets = dict(data)
datasets["execution_comparison"] = [{**row, "configuration": "v1 / 1×" if row["amplification"] == 1 else f'Dusk {row["amplification"]}×'} for row in data["executions"] if row["gross_quote"] == 10000 and row["fee_bps"] == 30]
datasets["headline_lvr"] = [{**row, "configuration": "v1 / 1×" if row["amplification"] == 1 else f'Dusk {row["amplification"]}×'} for row in data["scenarios"]]
# Independently recompute report metrics in SQLite from parameter/curve outputs.
# The original curve computation stays in model.py; this is material metric
# calculation and verification, not a fabricated query attached to Python data.
raw_rows = []
for row in datasets["execution_comparison"]:
    scenario = next(x for x in data["scenarios"] if x["amplification"] == row["amplification"])
    raw_rows.append((row['amplification'], row['capital_usd'], row['gross_quote'], row['base_out'], row['fee_bps'],
                     scenario['tail_depth_vs_v1'], scenario['shoulder_depth_vs_v1']))
sql = """-- Inputs: inspected Dusk curve modeled in model.py, $1m initial capital,
-- $10k gross buy, 30-bps fixed input fee, 100% annual volatility.
-- Tail/shoulder ratios and base_out are floating-point curve outputs.
WITH curve_outputs(amplification, capital_usd, gross_quote, base_out,
                   fee_bps, tail_depth_vs_v1, shoulder_depth_vs_v1) AS (
  VALUES\n""" + ",\n".join("    (" + ", ".join(repr(v) for v in row) + ")" for row in raw_rows) + """
)
SELECT *,
  CASE WHEN amplification=1 THEN 'v1 / 1×' ELSE 'Dusk ' || amplification || '×' END AS configuration,
  10000.0 * (gross_quote / base_out - 1) AS average_cost_bps,
  amplification * capital_usd * 1.0 / (8.0 * 365) AS gross_lvr_usd_day,
  1.0 - 1.0/amplification AS same_peak_depth_capital_saved
FROM curve_outputs
ORDER BY amplification;
"""
(ROOT / "verification_query.sql").write_text(sql)
connection = sqlite3.connect(":memory:")
connection.row_factory = sqlite3.Row
verified_rows = [dict(row) for row in connection.execute(sql).fetchall()]
for a, b in zip(verified_rows, datasets['execution_comparison']):
    assert abs(a['average_cost_bps'] - b['average_cost_bps']) < 1e-8
for a, b in zip(verified_rows, datasets['headline_lvr']):
    assert abs(a['gross_lvr_usd_day'] - b['gross_lvr_usd_day']) < 1e-8
datasets['verified_metrics'] = verified_rows
sources.append({"id": "verified", "label": "Independent metric calculation from modeled curve outputs", "path": "verification_query.sql",
                "query": {"engine": "SQLite", "language": "sql", "sql": sql, "executed_at": timestamp,
                          "description": "Actual executed cross-check of report metrics. Input outputs are computed by model.py, not observed trades. Original curve method and all scenarios are preserved in model.py and results.json.",
                          "tables_used": ["curve_outputs (inline CTE from model.py)", "model.py", "results.json"],
                          "metric_definitions": sources[0]['query']['metric_definitions'], "filters": sources[0]['query']['filters']}})
charts = []
for identity, label, dataset, field, axis, question in [
    ("execution", "Average cost of a $10,000 buy", "execution_comparison", "average_cost_bps", "Average cost (bps)", "How does concentration affect same-size execution?"),
    ("lvr", "Gross local LVR benchmark", "headline_lvr", "gross_lvr_usd_day", "USD per day", "How does amplification affect local gross adverse-selection exposure?")]:
    charts.append({"id": identity, "title": label, "subtitle": "$1m initial capital; centered synthetic scenarios", "showDescription": True,
                   "type": "bar", "intent": "comparison", "question": question,
                   "rationale": "Four discrete modeled configurations; zero-baseline bars and direct category labels allow magnitude comparison.",
                   "dataset": "verified_metrics", "sourceId": "verified", "layout": "full",
                   "encodings": {"x": {"field": "configuration", "type": "nominal"}, "y": {"field": field, "type": "quantitative"}},
                   "xAxisTitle": "Configuration", "yAxisTitle": axis, "valueFormat": "number"})
manifest = {"version": 1, "surface": "report", "title": title, "generatedAt": timestamp,
            "blocks": blocks, "sources": sources, "charts": charts,
            "tables": [{"id": "regions", "title": "Depth across curve regions", "dataset": "verified_metrics", "sourceId": "verified",
                        "defaultSort": {"field": "amplification", "direction": "asc"}, "density": "spacious",
                        "columns": [{"field": "amplification", "label": "Core depth (×)", "format": "number"},
                                    {"field": "shoulder_depth_vs_v1", "label": "Shoulder depth (×)", "format": "number"},
                                    {"field": "tail_depth_vs_v1", "label": "Tail depth (×)", "format": "number"},
                                    {"field": "same_peak_depth_capital_saved", "label": "Capital saved at peak", "format": "percent"}]}]}
payload = {"surface": "report", "manifest": manifest,
           "snapshot": {"version": 1, "generatedAt": timestamp, "status": "ready", "datasets": datasets}, "sources": sources}
payload = extend_artifact(payload, ROOT)
(ROOT / "artifact.json").write_text(json.dumps(payload, indent=2) + "\n")

source_files = [REPO / file for file in sources[1]["query"]["tables_used"]] + [V1 / file for file in sources[2]["query"]["tables_used"]]
source_files += [REPO/'programs/dusk/src/transitions/ledger.rs', ROOT/'model.py', ROOT/'dynamic_fees.py']
notes = {"generated_at": timestamp, "dusk_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip(),
         "v1_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=V1, text=True).strip(),
         "files": [{"path": str(p), "sha256": hashlib.sha256(p.read_bytes()).hexdigest()} for p in source_files],
         "source_authority": "Current local code controls mechanics; documentation adds intent. Research supplies benchmark definitions. Scenario numbers are original calculations.",
         "report_audience": "product stakeholders", "report_structure": "Title, executive summary, definitions, findings with charts/table, parameter/replay recommendations, further questions, caveats.",
         "chart_contract": "Two original full-width comparison bar charts plus a grouped dynamic-loss chart with five profiles and two ownership treatments. These are discrete magnitude comparisons, with zero baselines and category/legend labels. Tables support exact region and fee/capital lookup. No fabricated time series. Direct Codex visual inspection is unavailable; report schema and data are checked.",
         "notebook_execution": "All code cells executed sequentially in one fresh Python namespace, outputs captured. nbformat/nbclient unavailable; no Jupyter kernel execution or independent nbformat schema validation.",
         "replay_not_claimed": ["No real market performance estimates", "No historically calibrated dynamic-fee effectiveness; conditional formula experiments included", "No active-hLP or on-chain quote parity certification"]}
(ROOT / "source-notes.json").write_text(json.dumps(notes, indent=2) + "\n")

cells = []
def markdown(text):
    cells.append({"cell_type": "markdown", "id": uuid.uuid4().hex[:8], "metadata": {}, "source": text.splitlines(keepends=True)})
def code(text):
    cells.append({"cell_type": "code", "id": uuid.uuid4().hex[:8], "metadata": {}, "source": text.splitlines(keepends=True), "execution_count": None, "outputs": []})
markdown("# Dusk versus Omnipair v1\n\n## tl;dr\n5× centered depth saves 80% of capital for the same local depth but multiplies local gross LVR per dollar by five. At $1m and 100% annual volatility: $342/day versus $1,712/day. These are local benchmarks, not measured returns. Market-wide dynamic-fee effectiveness remains uncalibrated.")
markdown("## Context & Methods\nCompanion to the report.\n\n### Key Assumptions\nBalanced center price=1; ordinary reserve NAV is capital; full cash, no active hLP or public borrowing, frozen center. Real-valued source-derived nested curve; no atom parity claim. Fixed fee experiments do not implement the dynamic schedule.\n\nSources: Dusk curve.rs at 26bc75b217a96a6241b82b05cd52c1b587272dc1; v1 swap.rs at 84a8ef7a2a3a81e750435b86d7191bf43413d6a4; https://arxiv.org/html/2208.06046v5 and https://arxiv.org/abs/2305.14604. Local source hashes: source-notes.json.")
code("from pathlib import Path\nimport sys, json\nanalysis_dir = Path.cwd()\nif not (analysis_dir / 'model.py').exists():\n    analysis_dir = analysis_dir / 'analysis' / 'capital-efficiency'\nsys.path.insert(0, str(analysis_dir))\nfrom model import Curve, build_results, validate_model\n")
markdown("## Data\nSynthetic parameter grid. Change this cell to rerun the model; regenerate the report separately if assumptions change.")
code("parameters = dict(capital=1_000_000, annual_volatility=1.0, core_bps=100, fade_bps=400, fee_bps=30, lp_fee_share=0.8)\nresults = build_results(**parameters)\nprint(json.dumps(parameters, indent=2))")
markdown("## Results\n### Centered capital and gross local LVR")
code("for row in results['scenarios']:\n    print(f\"A={row['amplification']:>2}: gross LVR ${row['gross_lvr_usd_day']:,.2f}/day; equal-peak capital ${row['same_peak_depth_capital_usd']:,.0f}; tail {row['tail_depth_vs_v1']:.3f}x\")")
markdown("### Same-size execution and toxic jump")
code("for row in results['executions']:\n    if row['gross_quote'] == 10000 and row['fee_bps'] == parameters['fee_bps']:\n        print(f\"A={row['amplification']:>2}: $10k buy cost {row['average_cost_bps']:.2f} bps\")\nfor row in results['shocks']:\n    if row['jump'] == 0.01 and row['fee_bps'] == parameters['fee_bps']:\n        print(f\"A={row['amplification']:>2}: +1% jump LP shortfall ${row['lp_net_shortfall']:.2f}\")")
markdown("### Validation\nChecks centered NAV, derivative/gamma, equal-depth LVR, reciprocal symmetry, boundary continuity, fee conservation, CPMM closed forms and infeasible width/amplification rejection.")
code("checks = validate_model()\nprint(chr(10).join(checks))")
markdown("### Dynamic fee extension\nThe current input-denominated gross-path distance and volatility formulas are modeled in dynamic_fees.py. Example coefficients: distance=10, volatility=1; base=30bps; each surcharge cap=200bps; half-life=60s; protocol takes 20% of base only. Prior accumulators 0/0.005/0.01 are conditional states, not annualized volatility. Center fixed, no active hLP, cash restrictions or compounding. Real-valued arithmetic; not integer program parity.")
code("from dynamic_fees import experiments, validate as validate_dynamic\ndynamic = experiments()\nfor row in dynamic['dynamic_shocks']:\n    if row['external_jump'] == 0.01:\n        print(f\"{row['profile']}: loss distributed ${row['lp_loss_distributed_usd']:.2f}; retained ${row['lp_loss_retained_usd']:.2f}\")\nprint('Dynamic checks:', len(validate_dynamic()))")
markdown("### Capital after paying dynamic fees\nThis is a separate finite-size execution experiment: resize capital to match v1's $10k buy at 230.09bps all-in. The shock losses above continue to use equal $1m capital.")
code("for row in dynamic['dynamic_execution']:\n    print(f\"{row['profile']}: cost {row['average_cost_bps']:.2f}bps; matched-execution capital ${row['capital_for_v1_execution_usd']:,.0f}; saving {row['capital_saved_same_execution']:.1%}\")")
markdown("## Takeaways\n1. Choose depth targets at actual order size and fee, not peak multiplier alone.\n2. At A=5, residual toxic-cost factor must fall by more than 80% versus v1 to reduce net toxic cost per capital at the center. Organic fee revenue can change net profitability.\n3. Replay the same external opportunities with each engine's actual dynamic fees, center policy, cash constraints and hLP settlement before selecting parameters.\n\nExecution note: cells were executed sequentially in Python with captured outputs; a Jupyter kernel and nbformat validator were unavailable. Rerun in Jupyter to certify that separate execution surface.")
namespace = {"__name__": "__main__"}
count = 0
for cell in cells:
    if cell["cell_type"] == "code":
        count += 1
        stream = io.StringIO()
        with contextlib.redirect_stdout(stream):
            exec(compile(''.join(cell["source"]), f"notebook-cell-{count}", "exec"), namespace)
        cell["execution_count"] = count
        cell["outputs"] = [{"output_type": "stream", "name": "stdout", "text": stream.getvalue().splitlines(keepends=True)}] if stream.getvalue() else []
notebook = {"nbformat": 4, "nbformat_minor": 5, "metadata": {"kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"}, "language_info": {"name": "python", "version": sys.version.split()[0]}}, "cells": cells}
(ROOT / "analysis.ipynb").write_text(json.dumps(notebook, indent=2) + "\n")
print(f"Built report payload and executed {count} notebook code cells.")
