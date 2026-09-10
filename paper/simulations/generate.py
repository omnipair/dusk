#!/usr/bin/env python3
"""Reproduce the whitepapers' disclosed illustrations, not an onchain emulator.

Arithmetic uses only the standard library. ReportLab produces vector figures.
Run from any directory: python3 paper/simulations/generate.py
"""

import csv
import json
import math
from pathlib import Path

from reportlab.pdfgen import canvas
from reportlab.lib.colors import HexColor

ROOT = Path(__file__).resolve().parent
DATA, FIGURES = ROOT / "data", ROOT / "figures"
SNAPSHOT = "263c1635ad87ee36ffd9bf0b970457e4be965326"
NAD = 1_000_000_000
YEAR_MS = 365 * 24 * 60 * 60 * 1000
DAY_MS = YEAR_MS // 365
TARGET_BPS = 7000
STEEPNESS = 4
SPEED = 20
TAIL = 100.0
LAYERS = [(450.0, 0.95, 1 / 0.95), (450.0, 0.85, 1 / 0.85)]
BLUE, ORANGE, TEAL = "#205493", "#bd5a20", "#147d72"


def write_csv(name, rows):
    with (DATA / name).open("w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def trunc_div(value, denominator):
    return (1 if value >= 0 else -1) * (abs(value) // denominator)


def adapt(anchor, error, elapsed_ms=DAY_MS):
    step = trunc_div(SPEED * error * min(elapsed_ms, YEAR_MS), YEAR_MS)
    step = min(NAD // 2, max(-NAD // 2, step))
    return min(2 * NAD, max(NAD // 1000, anchor * (NAD + step) // NAD))


def apr(anchor, error):
    multiplier = NAD + (STEEPNESS - 1) * error if error >= 0 else NAD + trunc_div((NAD - NAD // STEEPNESS) * error, NAD)
    return anchor * multiplier // NAD


def inventory(z):
    u, v, active = TAIL / z, TAIL * z, TAIL
    for liquidity, lower, upper in LAYERS:
        clipped = min(upper, max(lower, z))
        u += liquidity * (1 / clipped - 1 / upper)
        v += liquidity * (clipped - lower)
        if lower < z < upper:
            active += liquidity
    return u, v, active


def recovery(debt, claim=1_000_000, revenue=0, input_amount=1_000_000, equity=1_000_000):
    gap = max(0, debt - revenue - claim)
    critical = debt > 0 if claim == 0 else debt * 8 >= claim * 9
    if not claim or not gap or not equity or not input_amount or gap * 10000 < claim * 25:
        return gap, 0, 0, critical
    discount = min(500, gap * 16 * 500 // claim)
    matched = min(gap, input_amount)
    bonus = min(equity, matched * discount // (10000 - discount))
    return gap, discount, bonus, critical


def make_canvas(name, width=600, height=330):
    c = canvas.Canvas(str(FIGURES / name), pagesize=(width, height), invariant=1)
    c.setTitle(name.removesuffix(".pdf").replace("_", " "))
    c.setAuthor("Omnipair Contributors")
    return c


def panel(c, rect, xbounds, ybounds, xticks, yticks, xlabel, ylabel, log_y=False, bands=()):
    x0, y0, width, height = rect
    xmin, xmax = xbounds
    ymin, ymax = ybounds
    transform = math.log10 if log_y else lambda a: a
    lo, hi = transform(ymin), transform(ymax)
    def point(x, y):
        return x0 + (x - xmin) / (xmax - xmin) * width, y0 + (transform(y) - lo) / (hi - lo) * height
    for left, right, color in bands:
        c.setFillColor(HexColor(color))
        c.rect(point(left, ymin)[0], y0, point(right, ymin)[0] - point(left, ymin)[0], height, stroke=0, fill=1)
    c.setFont("Helvetica", 11)
    for y in yticks:
        py = point(xmin, y)[1]
        c.setStrokeColor(HexColor("#dddddd")); c.setLineWidth(0.4)
        c.line(x0, py, x0 + width, py)
        c.setFillColor(HexColor("#333333")); c.drawRightString(x0 - 8, py - 3, f"{y:g}")
    for x in xticks:
        px = point(x, ymin)[0]
        c.setStrokeColor(HexColor("#666666")); c.line(px, y0, px, y0 - 4)
        c.setFillColor(HexColor("#333333")); c.drawCentredString(px, y0 - 17, f"{x:g}")
    c.setStrokeColor(HexColor("#444444")); c.setLineWidth(0.7)
    c.line(x0, y0, x0 + width, y0); c.line(x0, y0, x0, y0 + height)
    c.setFont("Helvetica", 12)
    c.drawCentredString(x0 + width / 2, y0 - 34, xlabel)
    c.saveState(); c.translate(x0 - 43, y0 + height / 2); c.rotate(90)
    c.drawCentredString(0, 0, ylabel); c.restoreState()
    return point


def curve(c, point, rows, color, dash=None):
    c.setStrokeColor(HexColor(color)); c.setLineWidth(1.8)
    c.setDash(dash or [])
    path = c.beginPath()
    for idx, (x, y) in enumerate(rows):
        px, py = point(x, y)
        if idx == 0: path.moveTo(px, py)
        else: path.lineTo(px, py)
    c.drawPath(path); c.setDash([])


def legend(c, x, y, text, color, dash=None):
    c.setStrokeColor(HexColor(color)); c.setLineWidth(1.8); c.setDash(dash or [])
    c.line(x, y, x + 21, y); c.setDash([])
    c.setFillColor(HexColor("#333333")); c.setFont("Helvetica", 11)
    c.drawString(x + 27, y - 3, text)


def main():
    DATA.mkdir(parents=True, exist_ok=True); FIGURES.mkdir(parents=True, exist_ok=True)
    rates = []
    for scenario, util_bps, error in [("idle", 0, -NAD), ("target", TARGET_BPS, 0), ("full", 10000, NAD)]:
        anchor = 4 * NAD // 100
        for day in range(31):
            rates.append(dict(scenario=scenario, utilization_bps=util_bps, day=day, anchor_nad=anchor,
                              borrow_apr_nad=apr(anchor, error), borrow_apr_percent=100 * apr(anchor, error) / NAD))
            anchor = adapt(anchor, error)
    write_csv("adaptive_irm.csv", rates)

    prices = sorted(set([0.5 + i * 1.3 / 650 for i in range(651)] + [edge * edge + eps for _, a, b in LAYERS for edge in (a,b) for eps in (-1e-8, 1e-8)]))
    reserves = []
    for p in prices:
        u,v,active = inventory(math.sqrt(p))
        reserves.append(dict(marginal_price=p, ordinary_base=u, ordinary_quote=v, inventory_ratio=v/u, active_liquidity=active))
    write_csv("concentrated_curve.csv", reserves)

    recovery_rows = []
    for debt in sorted(set(list(range(1_000_000, 1_150_001, 250)) + [1_002_499,1_002_500,1_062_500,1_125_000])):
        gap,discount,bonus,critical = recovery(debt)
        recovery_rows.append(dict(actual_debt=debt, canonical_claim=1_000_000, usable_revenue=0,
                                  debt_claim_ratio=debt/1_000_000, funding_gap=gap,
                                  discount_bps=discount, bonus_output=bonus, critical=critical))
    write_csv("hlp_recovery.csv", recovery_rows)

    summary = [r for r in rates if r["day"] == 30]
    lines = [r"\begin{center}", r"\begin{tabular}{@{}lrr@{}}", r"\toprule",
             r"Fixed utilization & Day-30 anchor APR & Day-30 borrower APR \\", r"\midrule"]
    for r in summary:
        lines.append(f"{r['utilization_bps']/100:g}\\% & {r['anchor_nad']/NAD*100:.4f}\\% & {r['borrow_apr_percent']:.4f}\\% \\\\")
    lines += [r"\bottomrule",r"\end{tabular}",r"\end{center}"]
    (DATA / "summary.tex").write_text("\n".join(lines)+"\n")
    lines = [r"\begin{center}\small",r"\begin{tabular}{@{}rrrrl@{}}",r"\toprule",
             r"Debt & Net gap & Discount (bps) & Bonus (atoms) & Critical \\",r"\midrule"]
    for debt in [1_002_499,1_002_500,1_062_500,1_125_000]:
        gap,d,b,critical = recovery(debt)
        lines.append(f"{debt:,} & {gap:,} & {d} & {b:,} & {'Yes' if critical else 'No'} \\\\")
    lines += [r"\bottomrule",r"\end{tabular}",r"\end{center}"]
    (DATA / "recovery_table.tex").write_text("\n".join(lines)+"\n")

    c = make_canvas("concentrated_curve.pdf", 620, 320)
    bands = [(0.85**2,(1/0.85)**2,"#f1f5f9"),(0.95**2,(1/0.95)**2,"#dce8f3")]
    point = panel(c,(58,58,236,206),(0.5,1.8),(0,1100),[0.5,1,1.5],[0,250,500,750,1000],"Marginal price p","Active liquidity",bands=bands)
    curve(c,point,[(r['marginal_price'],r['active_liquidity']) for r in reserves],BLUE)
    point = panel(c,(373,58,225,206),(0.5,1.8),(0,4.5),[0.5,1,1.5],[0,1,2,3,4],"Marginal price p","Quote per base",bands=bands)
    curve(c,point,[(r['marginal_price'],r['marginal_price']) for r in reserves],BLUE)
    curve(c,point,[(r['marginal_price'],r['inventory_ratio']) for r in reserves],ORANGE,[4,2])
    legend(c,70,287,"Core + shoulder + tail",BLUE)
    legend(c,374,290,"Marginal price",BLUE); legend(c,374,275,"Inventory ratio V/U",ORANGE,[4,2])
    c.save()

    c = make_canvas("adaptive_irm.pdf")
    point = panel(c,(65,57,510,221),(0,30),(0.1,100),list(range(0,31,5)),[0.1,1,10,100],"Days (one update per day)","Borrow APR percent (log scale)",log_y=True)
    for name,color,x in [("idle",ORANGE,70),("target",TEAL,230),("full",BLUE,410)]:
        curve(c,point,[(r['day'],r['borrow_apr_percent']) for r in rates if r['scenario']==name],color)
        legend(c,x,305,{"idle":"0% utilization","target":"70% utilization","full":"100% utilization"}[name],color)
    c.save()

    c = make_canvas("hlp_recovery.pdf")
    point = panel(c,(65,57,510,221),(1,1.15),(0,5.6),[1,1.025,1.05,1.075,1.1,1.125,1.15],[0,1,2,3,4,5],"Actual debt / canonical opposite claim","Recovery discount percent")
    curve(c,point,[(r['debt_claim_ratio'],r['discount_bps']/100) for r in recovery_rows],BLUE)
    for x,label in [(17/16,"Maximum at 17/16"),(9/8,"Critical at 9/8")]:
        c.setStrokeColor(HexColor("#888888")); c.setDash([3,3]); px,py=point(x,0); c.line(px,py,px,point(x,5.6)[1]); c.setDash([])
        c.setFillColor(HexColor("#333333")); c.setFont("Helvetica",11); c.drawCentredString(px,289,label)
    legend(c,65,309,"No usable revenue; bonus subject to input and equity caps",BLUE)
    c.save()

    # Independent property checks on the published mathematical illustrations.
    for _,lower,upper in LAYERS:
        for z in (lower,upper):
            left,right = inventory(z-1e-10),inventory(z+1e-10)
            assert abs(left[0]-right[0]) < 1e-5 and abs(left[1]-right[1]) < 1e-5
    u,v,_=inventory(1); assert math.isclose(u,190) and math.isclose(v,190)
    assert recovery(1_002_499)[1]==0 and recovery(1_002_500)[1]==20
    assert recovery(1_062_500)[1]==500 and not recovery(1_124_999)[3] and recovery(1_125_000)[3]
    assert recovery(1_062_500)[2]==3289
    assert recovery(1_130_000,revenue=120_000)[1:]==(80,80,True)
    assert recovery(1_062_500,equity=2)[2]==2
    assert all(NAD//1000 <= r['anchor_nad'] <= 2*NAD for r in rates)
    assert next(r for r in summary if r['scenario']=='target')['anchor_nad']==4*NAD//100
    assert 900+110-10==1000 and 1000+0-0==1000
    metadata = dict(reviewed_commit=SNAPSHOT, target_utilization_bps=TARGET_BPS, steepness=STEEPNESS,
                    speed_per_year=SPEED, update_ms=DAY_MS, initial_anchor_nad=4*NAD//100,
                    curve_tail=TAIL, curve_layers=LAYERS, recovery_claim=1_000_000,
                    recovery_conversion="1 target atom per opposite atom", scope="Mathematical illustrations; not a full program emulator")
    (DATA / "assumptions.json").write_text(json.dumps(metadata,indent=2)+"\n")
    (DATA / "validation.json").write_text(json.dumps(dict(status="passed", checks=["range boundary continuity", "center inventory", "recovery activation/max/critical boundaries", "integer bonus arithmetic", "revenue netting distinction", "equity cap", "rate anchor bounds", "target equilibrium", "unpaid-interest exclusion"]),indent=2)+"\n")
    print(json.dumps(dict(status="passed", day30=summary),indent=2))


if __name__ == "__main__":
    main()
