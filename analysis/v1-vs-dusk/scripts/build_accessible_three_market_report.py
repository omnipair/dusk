#!/usr/bin/env python3
"""Build the answer-first report from individually available benchmark fields.

This deliberately does not manufacture a combined candidate score.  It reports
only fields whose artifact-level availability is explicit and on which the two
replay arms agree exactly.
"""

from __future__ import annotations

import hashlib
import html
import json
from datetime import datetime, timezone
from fractions import Fraction
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
RUN_ID = "slot-440459364-06c0e05aadb7e883"
RUN_DIR = ROOT / "results" / "runs" / RUN_ID
CACHE_DIR = ROOT / "results" / "scoped-three-market" / "default-baseline-cache"
REPORT_DIR = ROOT / "results" / "scoped-three-market" / "report"
EVIDENCE_DIR = REPORT_DIR / "evidence"
HTML_PATH = REPORT_DIR / "dusk-vs-v1-three-market.html"
NOTEBOOK_PATH = REPORT_DIR / "scoped-benchmark.ipynb"
EVIDENCE_PATH = EVIDENCE_DIR / "accessible-benchmark-results.json"

MARKETS = {
    "8cWJ9U2jFZhugi5CSvjN38eJrKuQx1AKth8XsvR6857p": "Fartcoin / USDC",
    "Ep18GzZwRzt5EunVprg4aMxBFYimxZv13mr2J24MMtFo": "stORE / USDC",
    "BZi9iPbcpLWDkHs8nNQe5pb9MxLQe8fbgHtypj2pL4UB": "OMFG / USDC",
}


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, value) -> None:
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fraction(value: dict) -> Fraction:
    if "availability" in value:
        if value["availability"] != "available" or value["value"] is None:
            raise RuntimeError(f"required value is unavailable: {value}")
        value = value["value"]
    return Fraction(int(value["numerator"]), int(value["denominator"]))


def usd(value: dict) -> Fraction:
    return fraction(value) / 1_000_000


def pct_delta(candidate: dict, factual: dict) -> Fraction:
    factual_value = fraction(factual)
    if factual_value == 0:
        raise RuntimeError("percentage denominator is zero")
    return (fraction(candidate) / factual_value - 1) * 100


def decimal(value: Fraction, places: int = 2) -> str:
    return f"{float(value):,.{places}f}"


def source_binding(path: Path) -> dict:
    return {
        "path": str(path.relative_to(ROOT)),
        "bytes": path.stat().st_size,
        "sha256": sha256_file(path),
    }


def economic_lanes(score: dict, venue: str) -> dict[str, Fraction]:
    lanes: dict[str, Fraction] = {}
    for row in score["revenue_yield"]["lane_totals"]:
        if (
            row["venue"] == venue
            and row["origin"] == "factual_user_flow"
            and row["accounting_class"] == "economic_revenue"
        ):
            availability = row["usd_micro"]["availability"]
            if availability.startswith("not_applicable"):
                continue
            if availability != "available":
                raise RuntimeError(f"economic lane unavailable: {row}")
            lanes[row["lane"]] = usd(row["usd_micro"])
    return lanes


def build_notebook(evidence: dict) -> None:
    expected_profile = evidence["scope"]["dusk_profile_id"]
    notebook = {
        "cells": [
            {
                "cell_type": "markdown",
                "metadata": {},
                "source": [
                    "# V1 vs Dusk defaults — accessible three-market evidence\n",
                    "\n",
                    "This notebook verifies the report inputs. It does not turn unavailable spot or leverage metrics into estimates.\n",
                ],
            },
            {
                "cell_type": "code",
                "execution_count": None,
                "metadata": {},
                "outputs": [],
                "source": [
                    "import json\n",
                    "from pathlib import Path\n",
                    "e = json.loads(Path('evidence/accessible-benchmark-results.json').read_text())\n",
                    f"assert e['scope']['dusk_profile_id'] == '{expected_profile}'\n",
                    "assert e['scope']['arms_agree_exactly'] is True\n",
                    "assert e['questions']['spot_capital_efficiency']['available'] is False\n",
                    "assert e['questions']['leverage']['full_campaign_available'] is False\n",
                    "assert e['questions']['collateral_efficiency']['actionable_headroom_delta_pct'] < 0\n",
                    "assert e['questions']['lp_protocol_economics']['economic_revenue_delta_pct'] < 0\n",
                    "print(e['decision']['recommendation'])\n",
                    "print('Actionable headroom delta:', round(e['questions']['collateral_efficiency']['actionable_headroom_delta_pct'], 2), '%')\n",
                    "print('Economic revenue delta:', round(e['questions']['lp_protocol_economics']['economic_revenue_delta_pct'], 3), '%')\n",
                ],
            },
            {
                "cell_type": "markdown",
                "metadata": {},
                "source": [
                    "## Interpretation\n",
                    "\n",
                    "V1 wins the accessible capacity and economic-revenue comparisons. Dusk defaults do not have enough comparable spot or full leverage evidence for an advantage claim.\n",
                ],
            },
        ],
        "metadata": {
            "kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
            "language_info": {"name": "python", "version": "3"},
        },
        "nbformat": 4,
        "nbformat_minor": 5,
    }
    write_json(NOTEBOOK_PATH, notebook)


def build_html(evidence: dict) -> str:
    q = evidence["questions"]
    collateral = q["collateral_efficiency"]
    spot = q["spot_capital_efficiency"]
    leverage = q["leverage"]
    economics = q["lp_protocol_economics"]
    constraints = collateral["dusk_limiting_constraints"]
    max_constraint = max(row["observations"] for row in constraints)
    constraint_rows = "".join(
        f"""<div class="bar-row"><span>{html.escape(row['label'])}</span><div class="track"><i style="width:{100 * row['observations'] / max_constraint:.1f}%"></i></div><strong>{row['observations']:,}</strong></div>"""
        for row in constraints
    )
    market_rows = "".join(
        f"""<tr><td><strong>{html.escape(row['label'])}</strong><small>{html.escape(row['role'])}</small></td><td>{row['borrow_observations']:,}</td><td>{row['liquidations']:,}</td><td>{decimal(Fraction(str(row['v1_revenue_usd'])), 2)}</td><td>{decimal(Fraction(str(row['dusk_revenue_usd'])), 3)}</td><td>{row['missing_capital_price_keys']}</td></tr>"""
        for row in evidence["markets"]
    )
    source_rows = "".join(
        f"<li><code>{html.escape(name)}</code> — <code>{binding['sha256'][:16]}…</code>, {binding['bytes']:,} bytes</li>"
        for name, binding in evidence["sources"].items()
    )
    generated = html.escape(evidence["generated_at"])
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>V1 vs Dusk defaults — accessible three-market evidence</title>
<style>
:root{{--ink:#111827;--muted:#596579;--paper:#f6f7f9;--card:#fff;--line:#d8dee8;--blue:#1d4ed8;--navy:#0b1f3a;--red:#b42318;--green:#067647;--amber:#b54708}}
*{{box-sizing:border-box}} body{{margin:0;background:var(--paper);color:var(--ink);font:15px/1.55 Inter,ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}}
.page{{max-width:1120px;margin:auto;padding:42px 28px 70px}} .hero{{background:linear-gradient(135deg,#071a33,#153f74);color:white;border-radius:22px;padding:34px 38px;box-shadow:0 18px 44px #0b1f3a22}}
.eyebrow{{font-size:12px;font-weight:800;letter-spacing:.12em;text-transform:uppercase;color:#93c5fd}} h1{{font-size:40px;line-height:1.08;margin:8px 0 14px;max-width:880px}} .lede{{font-size:18px;max-width:900px;color:#dbeafe;margin:0}}
.decision{{margin-top:22px;padding:14px 16px;border-left:4px solid #60a5fa;background:#ffffff12;border-radius:8px;font-size:17px}} .decision strong{{color:#fff}}
.stamp{{margin-top:18px;color:#bfdbfe;font-size:12px}} .grid4{{display:grid;grid-template-columns:repeat(4,1fr);gap:14px;margin:20px 0 30px}}
.kpi,.card{{background:var(--card);border:1px solid var(--line);border-radius:16px;box-shadow:0 6px 20px #0b1f3a0b}} .kpi{{padding:18px}} .kpi b{{display:block;font-size:27px;line-height:1.15;color:var(--navy)}} .kpi span{{display:block;color:var(--muted);font-size:12px;margin-top:7px}}
h2{{font-size:25px;margin:34px 0 14px;color:var(--navy)}} h3{{font-size:18px;margin:0 0 8px;color:var(--navy)}} .question-grid{{display:grid;grid-template-columns:1fr 1fr;gap:16px}} .card{{padding:20px}}
.badge{{display:inline-block;font-size:11px;font-weight:800;letter-spacing:.06em;text-transform:uppercase;border-radius:999px;padding:4px 9px;margin-bottom:10px}} .win{{color:var(--green);background:#ecfdf3}} .na{{color:var(--amber);background:#fff7ed}} .loss{{color:var(--red);background:#fef3f2}}
.metric{{display:flex;justify-content:space-between;gap:15px;border-top:1px solid #edf0f4;padding:9px 0}} .metric:first-of-type{{border-top:0}} .metric span{{color:var(--muted)}} .metric strong{{text-align:right}}
table{{width:100%;border-collapse:collapse;background:#fff;border:1px solid var(--line);border-radius:14px;overflow:hidden}} th{{text-align:left;background:#eef3f9;color:#334155;font-size:12px;text-transform:uppercase;letter-spacing:.04em}} th,td{{padding:11px 12px;border-bottom:1px solid #e6eaf0}} td small{{display:block;color:var(--muted)}} tr:last-child td{{border-bottom:0}}
.bar-row{{display:grid;grid-template-columns:150px 1fr 48px;align-items:center;gap:12px;margin:10px 0}} .track{{height:10px;background:#e8edf4;border-radius:99px;overflow:hidden}} .track i{{display:block;height:100%;background:linear-gradient(90deg,#2563eb,#60a5fa);border-radius:99px}}
.note{{border-left:4px solid #f59e0b;background:#fffbeb;padding:14px 16px;border-radius:8px;color:#78350f}} .sources{{font-size:12px;color:var(--muted)}} code{{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.92em}}
.footer{{margin-top:30px;padding-top:18px;border-top:1px solid var(--line);font-size:12px;color:var(--muted)}}
@media(max-width:800px){{.grid4,.question-grid{{grid-template-columns:1fr 1fr}}h1{{font-size:31px}}.page{{padding:20px 14px 44px}}.hero{{padding:25px 22px}}table{{font-size:12px}}th,td{{padding:8px 7px}}}}
@media(max-width:520px){{.grid4,.question-grid{{grid-template-columns:1fr}}.bar-row{{grid-template-columns:110px 1fr 42px}}}}
@media print{{body{{background:#fff;font-size:11px}}.page{{max-width:none;padding:0}}.hero{{border-radius:0;box-shadow:none;break-after:avoid}}.grid4{{gap:8px;margin:12px 0 18px}}.kpi,.card{{box-shadow:none}}h2{{margin:20px 0 9px;break-after:avoid}}.card,table,.note{{break-inside:avoid}}.question-grid{{gap:10px}}a{{color:inherit;text-decoration:none}}}}
</style></head><body><main class="page">
<section class="hero"><div class="eyebrow">Three-market selected panel · checked-in prospective Dusk defaults</div><h1>Keep V1. Dusk defaults do not beat the accessible historical evidence.</h1><p class="lede">The exact replay now completes, but the combined benchmark score remains unavailable. The individually available fields still show materially lower Dusk borrowing capacity and economic revenue; spot and full leverage comparisons remain unavailable and are not estimated.</p><div class="decision"><strong>Decision:</strong> do not replace V1 with the checked-in Dusk defaults. Continue Dusk tuning only if a later frozen profile clears the missing spot, leverage, capital-window, and external-state evidence.</div><div class="stamp">Generated {generated} · run {RUN_ID} · profile {evidence['scope']['dusk_profile_id'][:12]}… · both replay arms agree</div></section>

<section class="grid4"><div class="kpi"><b>{collateral['actionable_headroom_delta_pct']:.2f}%</b><span>Dusk actionable additional headroom vs V1</span></div><div class="kpi"><b>{collateral['actionable_total_capacity_delta_pct']:.2f}%</b><span>Dusk actionable total debt capacity vs V1</span></div><div class="kpi"><b>${economics['v1_economic_revenue_usd']:,.2f}</b><span>V1 factual-user-flow economic revenue</span></div><div class="kpi"><b>${economics['dusk_economic_revenue_usd']:.3f}</b><span>Dusk factual-user-flow economic revenue</span></div></section>

<h2>The four original questions</h2><section class="question-grid">
<article class="card"><span class="badge win">1 · V1 wins accessible evidence</span><h3>Collateral efficiency</h3><div class="metric"><span>Matched borrow prestates</span><strong>{collateral['matched_borrow_observations']:,}</strong></div><div class="metric"><span>Actionable additional headroom</span><strong>V1 ${collateral['v1_actionable_headroom_usd']:,.0f}<br>Dusk ${collateral['dusk_actionable_headroom_usd']:,.0f}</strong></div><div class="metric"><span>Actionable total capacity</span><strong>V1 ${collateral['v1_actionable_total_capacity_usd']:,.0f}<br>Dusk ${collateral['dusk_actionable_total_capacity_usd']:,.0f}</strong></div><div class="metric"><span>Observed debt increase filled</span><strong>Dusk {collateral['dusk_to_v1_debt_increase_pct']:.2f}% of V1</strong></div><p>Dusk left ${collateral['dusk_unmet_borrow_usd']:,.0f} of matched borrow demand unmet.</p></article>
<article class="card"><span class="badge na">2 · Not available</span><h3>Spot capital efficiency</h3><div class="metric"><span>Comparable swap notional</span><strong>${spot['comparable_swap_notional_usd']:.0f}</strong></div><div class="metric"><span>Execution cost / slippage</span><strong>N/A</strong></div><div class="metric"><span>Standardized probes</span><strong>N/A</strong></div><p>The defaults produced no comparable priced swap-execution sample. That prevents any Dusk spot advantage claim; it is not evidence that execution cost is zero.</p></article>
<article class="card"><span class="badge na">3 · Capacity result only</span><h3>Leverage and margin safety</h3><div class="metric"><span>Full loop / unwind campaign</span><strong>N/A</strong></div><div class="metric"><span>Dusk gross capacity vs V1 actionable</span><strong>{leverage['dusk_gross_vs_v1_actionable_pct']:.2f}%</strong></div><div class="metric"><span>Dusk actionable vs V1 actionable</span><strong>{leverage['dusk_actionable_vs_v1_actionable_pct']:.2f}%</strong></div><div class="metric"><span>Observed bad debt</span><strong>V1 $0 · Dusk $0</strong></div><p>V1 wins executable capacity. Zero bad debt is reassuring but does not prove better Dusk margin safety; adverse-path and full-unwind evidence is unavailable.</p></article>
<article class="card"><span class="badge win">4 · V1 wins accessible evidence</span><h3>LP and protocol economics</h3><div class="metric"><span>Economic revenue</span><strong>V1 ${economics['v1_economic_revenue_usd']:,.2f}<br>Dusk ${economics['dusk_economic_revenue_usd']:.3f}</strong></div><div class="metric"><span>Dusk delta</span><strong>{economics['economic_revenue_delta_pct']:.3f}%</strong></div><div class="metric"><span>Dusk swap-fee revenue</span><strong>${economics['dusk_swap_fee_revenue_usd']:.2f}</strong></div><div class="metric"><span>Dusk public-interest revenue</span><strong>${economics['dusk_interest_revenue_usd']:.3f}</strong></div><p>Maximum-feasible hLP funding includes 77 deferred external-state operations, so hLP yield and complete capital-normalized APY remain N/A.</p></article>
</section>

<h2>Why Dusk capacity was lower</h2><section class="card">{constraint_rows}<p class="sources">Counts can overlap because multiple constraints may bind the same matched prestate. Cash and reduce-only each limited 0 observations.</p></section>

<h2>Market evidence</h2><table><thead><tr><th>Market</th><th>Borrow obs.</th><th>Liquidations</th><th>V1 revenue ($)</th><th>Dusk revenue ($)</th><th>Missing capital marks</th></tr></thead><tbody>{market_rows}</tbody></table>

<h2>What this verdict does—and does not—mean</h2><div class="note"><strong>Accessible-evidence decision, not global certification.</strong> The replay completed but <code>complete_for_scoring=false</code> because 77 external-state operations were deferred. The scorer also lacks a capital checkpoint for the first 64 seconds of the Fartcoin market window and has 93 counterfactual capital price-hour gaps. Therefore the combined score, execution-cost/slippage metrics, capital-normalized yield, and full leverage campaign remain unavailable. Individually available capacity, revenue, and bad-debt fields are reported because both replay arms agree exactly on them.</div>

<h2>Scope and sources</h2><ul class="sources"><li>Three report markets: Fartcoin/USDC (spot), stORE/USDC (borrowing), OMFG/USDC (liquidations).</li><li>Training window: 2026-04-15 20:00 UTC to 2026-06-30 19:20:34 UTC.</li><li>Dusk configuration: checked-in prospective program defaults, not a deployed production configuration and not a calibrated winner.</li><li>Underlying replay inputs contain five selected markets for required state continuity; every numerical headline above is filtered to the exact three-market report set.</li>{source_rows}</ul>
<div class="footer">Prepared from proof-bound local artifacts. No current-price substitution, interpolation, or invented unavailable metric is used.</div>
</main></body></html>"""


def main() -> None:
    EVIDENCE_DIR.mkdir(parents=True, exist_ok=True)
    arms: dict[str, tuple[Path, dict]] = {}
    for path in CACHE_DIR.glob("*.arm.json"):
        document = read_json(path)
        arms[document["arm"]] = (path, document)
    if set(arms) != {"fixed_flow", "intent"}:
        raise RuntimeError("expected exactly fixed-flow and intent arm evidence")
    fixed_path, fixed = arms["fixed_flow"]
    intent_path, intent = arms["intent"]
    fixed_score = fixed["score_evidence"]
    intent_score = intent["score_evidence"]
    expected_profile = "d34eb2a34ed5c388ee58546c3bd650df70d6def21f1aaa085a2ae05a7ea5f981"
    if fixed["profile_id"] != expected_profile or intent["profile_id"] != expected_profile:
        raise RuntimeError("arm evidence is not the checked-in Dusk defaults profile")
    if fixed_score["included_market_count"] != 3 or intent_score["included_market_count"] != 3:
        raise RuntimeError("arm evidence is not filtered to exactly three markets")
    if fixed_score["candidate_score"] is not None or intent_score["candidate_score"] is not None:
        raise RuntimeError("this report must not supersede a complete candidate score")
    if fixed["counterfactual"]["complete_for_scoring"] or intent["counterfactual"]["complete_for_scoring"]:
        raise RuntimeError("accessible-evidence report is only for typed incomplete replay")

    exact_paths = [
        "stakeholder_revenue_v1_usd_micro",
        "stakeholder_revenue_dusk_user_usd_micro",
        "v1_actionable_headroom_matched_observation_sum_usd_micro",
        "dusk_actionable_headroom_matched_observation_sum_usd_micro",
        "projected_total_debt_capacity",
        "v1_debt_increase_notional_usd_micro",
        "dusk_debt_increase_notional_usd_micro",
        "dusk_unmet_borrow_notional_usd_micro",
        "v1_bad_debt_usd_micro",
        "dusk_bad_debt_usd_micro",
        "dusk_capacity_decomposition",
    ]
    for key in exact_paths:
        if fixed_score[key] != intent_score[key]:
            raise RuntimeError(f"replay arms disagree on {key}")

    projected = fixed_score["projected_total_debt_capacity"]
    v1_headroom = fixed_score["v1_actionable_headroom_matched_observation_sum_usd_micro"]
    dusk_headroom = fixed_score["dusk_actionable_headroom_matched_observation_sum_usd_micro"]
    v1_total = projected["v1_actionable_max_debt_observation_sum_usd_micro"]
    dusk_gross_total = projected["dusk_gross_max_debt_observation_sum_usd_micro"]
    dusk_total = projected["dusk_actionable_max_debt_observation_sum_usd_micro"]
    v1_debt_increase = fixed_score["v1_debt_increase_notional_usd_micro"]
    dusk_debt_increase = fixed_score["dusk_debt_increase_notional_usd_micro"]
    v1_revenue = fixed_score["stakeholder_revenue_v1_usd_micro"]
    dusk_revenue = fixed_score["stakeholder_revenue_dusk_user_usd_micro"]
    dusk_lanes = economic_lanes(fixed_score, "dusk")
    dusk_swap_revenue = sum(
        value for lane, value in dusk_lanes.items() if "swap" in lane
    )
    dusk_interest_revenue = sum(
        value for lane, value in dusk_lanes.items() if "interest" in lane
    )
    constraints = fixed_score["dusk_capacity_decomposition"]["constraints"]
    labels = {
        "underwriting": "Underwriting",
        "global_health_floor": "Global health floor",
        "daily_borrow_bucket": "Daily borrow bucket",
        "cash": "Cash",
        "reduce_only": "Reduce-only",
    }

    market_by_arm = {
        arm: {row["market"]: row for row in document["score_evidence"]["markets"]}
        for arm, (_, document) in arms.items()
    }
    if set(market_by_arm["fixed_flow"]) != set(MARKETS):
        raise RuntimeError("fixed-flow market set differs from the report market set")
    market_rows = []
    roles = {
        "8cWJ9U2jFZhugi5CSvjN38eJrKuQx1AKth8XsvR6857p": "spot archetype",
        "Ep18GzZwRzt5EunVprg4aMxBFYimxZv13mr2J24MMtFo": "borrowing archetype",
        "BZi9iPbcpLWDkHs8nNQe5pb9MxLQe8fbgHtypj2pL4UB": "liquidation archetype",
    }
    for market in MARKETS:
        fixed_market = market_by_arm["fixed_flow"][market]
        intent_market = market_by_arm["intent"][market]
        for key in [
            "stakeholder_revenue_v1_usd_micro",
            "stakeholder_revenue_dusk_user_usd_micro",
            "projected_total_debt_capacity",
            "v1_bad_debt_usd_micro",
            "dusk_bad_debt_usd_micro",
        ]:
            if fixed_market[key] != intent_market[key]:
                raise RuntimeError(f"market arms disagree on {market}:{key}")
        market_rows.append(
            {
                "market": market,
                "label": MARKETS[market],
                "role": roles[market],
                "borrow_observations": fixed_market["projected_total_debt_capacity"]["matched_observation_count"],
                "liquidations": fixed_market["liquidation_count"],
                "v1_revenue_usd": float(usd(fixed_market["stakeholder_revenue_v1_usd_micro"])),
                "dusk_revenue_usd": float(usd(fixed_market["stakeholder_revenue_dusk_user_usd_micro"])),
                "missing_capital_price_keys": len(fixed_market["missing_price_keys"]),
            }
        )

    reconciliation_path = RUN_DIR / "v1-reconciliation.json"
    profile_path = next(CACHE_DIR.glob("*.profile.json"))
    evidence = {
        "schema_version": 1,
        "generated_at": datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
        "status": "accessible_evidence_only",
        "decision": {
            "recommendation": "Keep V1; do not replace it with the checked-in Dusk defaults.",
            "scope": "Decision on accessible three-market evidence, not a certified global winner.",
        },
        "scope": {
            "run_id": RUN_ID,
            "dusk_profile_id": expected_profile,
            "dusk_profile_basis": "checked-in prospective program defaults",
            "markets": list(MARKETS),
            "market_count": 3,
            "split": "training",
            "window_start_unix_seconds": fixed_score["window_start_unix_seconds"],
            "window_end_unix_seconds_exclusive": fixed_score["window_end_unix_seconds_exclusive"],
            "arms": ["fixed_flow", "intent"],
            "arms_agree_exactly": True,
            "hlp_scenario": fixed["hlp_scenario"],
        },
        "questions": {
            "collateral_efficiency": {
                "available": True,
                "verdict": "V1 wins the accessible matched-prestate comparison.",
                "matched_borrow_observations": projected["matched_observation_count"],
                "v1_actionable_headroom_usd": float(usd(v1_headroom)),
                "dusk_actionable_headroom_usd": float(usd(dusk_headroom)),
                "actionable_headroom_delta_pct": float(pct_delta(dusk_headroom, v1_headroom)),
                "v1_actionable_total_capacity_usd": float(usd(v1_total)),
                "dusk_gross_total_capacity_usd": float(usd(dusk_gross_total)),
                "dusk_actionable_total_capacity_usd": float(usd(dusk_total)),
                "actionable_total_capacity_delta_pct": float(pct_delta(dusk_total, v1_total)),
                "v1_debt_increase_usd": float(usd(v1_debt_increase)),
                "dusk_debt_increase_usd": float(usd(dusk_debt_increase)),
                "dusk_to_v1_debt_increase_pct": float(fraction(dusk_debt_increase) / fraction(v1_debt_increase) * 100),
                "dusk_unmet_borrow_usd": float(usd(fixed_score["dusk_unmet_borrow_notional_usd_micro"])),
                "dusk_limiting_constraints": [
                    {
                        "constraint": row["constraint"],
                        "label": labels[row["constraint"]],
                        "observations": row["limiting_matched_observation_count"],
                    }
                    for row in constraints
                ],
            },
            "spot_capital_efficiency": {
                "available": False,
                "verdict": "No Dusk advantage claim; comparable execution evidence is absent.",
                "comparable_swap_notional_usd": float(usd(fixed_score["factual_swap_input_notional_usd_micro"])),
                "volume_weighted_execution_cost_delta_ppb": fixed_score["volume_weighted_execution_cost_delta_ppb_exact"],
                "p95_slippage_delta_ppb": fixed_score["p95_slippage_delta_ppb"],
                "standardized_spot_probe_count": len(fixed_score["spot_probes"]),
            },
            "leverage": {
                "full_campaign_available": False,
                "verdict": "V1 wins executable debt capacity; relative margin safety is not established.",
                "dusk_gross_vs_v1_actionable_pct": float(fraction(dusk_gross_total) / fraction(v1_total) * 100),
                "dusk_actionable_vs_v1_actionable_pct": float(fraction(dusk_total) / fraction(v1_total) * 100),
                "v1_bad_debt_usd": float(usd(fixed_score["v1_bad_debt_usd_micro"])),
                "dusk_bad_debt_usd": float(usd(fixed_score["dusk_bad_debt_usd_micro"])),
                "tail_liquidation_shortfall_delta_usd_micro": fixed_score["tail_liquidation_shortfall_delta_usd_micro"],
                "global_health_unsatisfied_observations": fixed_score["dusk_capacity_decomposition"]["current_global_health_floor_unsatisfied_observation_count"],
            },
            "lp_protocol_economics": {
                "available": True,
                "verdict": "V1 wins available factual-user-flow economic revenue.",
                "v1_economic_revenue_usd": float(usd(v1_revenue)),
                "dusk_economic_revenue_usd": float(usd(dusk_revenue)),
                "economic_revenue_delta_pct": float(pct_delta(dusk_revenue, v1_revenue)),
                "dusk_swap_fee_revenue_usd": float(dusk_swap_revenue),
                "dusk_interest_revenue_usd": float(dusk_interest_revenue),
                "hlp_yield_available": False,
            },
        },
        "markets": market_rows,
        "limitations": {
            "complete_for_scoring": False,
            "combined_score": None,
            "deferred_external_state_operations": fixed["counterfactual"]["deferred_external_state_count"],
            "missing_counterfactual_capital_price_keys": len(fixed_score["missing_price_keys"]),
            "capital_window_seed_gap_seconds": 64,
            "blocking_reasons": fixed_score["blocking_reasons"],
            "not_reported": [
                "combined/global winner score",
                "spot execution cost and slippage",
                "standardized spot depth probes",
                "capital-normalized APY",
                "full leverage loop, adverse path, and unwind campaign",
                "complete hLP funding yield",
            ],
        },
        "sources": {
            "fixed_flow_arm": source_binding(fixed_path),
            "intent_arm": source_binding(intent_path),
            "combined_profile": source_binding(profile_path),
            "v1_reconciliation": source_binding(reconciliation_path),
        },
        "evidence_sha256": "",
    }
    sealed = dict(evidence)
    sealed["evidence_sha256"] = ""
    evidence["evidence_sha256"] = hashlib.sha256(
        json.dumps(sealed, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    write_json(EVIDENCE_PATH, evidence)
    HTML_PATH.write_text(build_html(evidence), encoding="utf-8")
    build_notebook(evidence)
    write_json(
        REPORT_DIR / "artifact.json",
        {
            "schema_version": 1,
            "title": "V1 vs Dusk defaults — accessible three-market evidence",
            "html": HTML_PATH.name,
            "notebook": NOTEBOOK_PATH.name,
            "evidence": str(EVIDENCE_PATH.relative_to(REPORT_DIR)),
            "generated_at": evidence["generated_at"],
            "decision": evidence["decision"],
        },
    )
    print(HTML_PATH)
    print(EVIDENCE_PATH)
    print(NOTEBOOK_PATH)


if __name__ == "__main__":
    main()
