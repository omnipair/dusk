from dataclasses import asdict, replace
from itertools import product
from pathlib import Path
import csv
import json
from model import Params, Simulation, SCENARIOS

ROOT = Path(__file__).parent
OUT = ROOT / "results"
OUT.mkdir(exist_ok=True)


def write_csv(name, rows):
    with (OUT/name).open("w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def screen():
    rows = []
    configurations = list(product((.02,.05,.10), (2,7,14), (.5,1.,2.), (.7,.8,.9)))
    for i,(target,half,cap,limit) in enumerate(configurations):
        p = Params(target=target, half_days=half, cap=cap, borrow_limit=limit)
        for scenario in SCENARIOS:
            result = Simulation(p,scenario).run()
            rows.append(dict(config=i, target=target, half_days=half, cap=cap,
                             borrow_limit=limit, **result))
        if (i+1)%27 == 0:
            print(f"{i+1}/{len(configurations)} configurations complete", flush=True)
    write_csv("screen.csv", rows)
    summaries=[]
    for i,(target,half,cap,limit) in enumerate(configurations):
        group=[r for r in rows if r["config"]==i]
        # Equal scenario weights are displayed, not an objective or probabilities.
        summaries.append(dict(config=i,target=target,half_days=half,cap=cap,borrow_limit=limit,
            avg_borrow=sum(r["avg_borrow"] for r in group)/len(group),
            mean_borrower_apr=sum(r["borrower_apr"] for r in group)/len(group),
            worst_senior_loss=max(r["senior_loss"] for r in group),
            worst_cash_backlog=max(r["withdrawal_backlog"] for r in group),
            worst_cash_wait=max(r["average_wait"] for r in group),
            mean_cap_days=sum(r["cap_days"] for r in group)/len(group),
            mean_rate_turns=sum(r["rate_turns"] for r in group)/len(group)))
    for row in summaries:
        row["pareto"] = not any(
            other["avg_borrow"] >= row["avg_borrow"] and
            other["mean_borrower_apr"] <= row["mean_borrower_apr"] and
            other["worst_senior_loss"] <= row["worst_senior_loss"] and
            other["worst_cash_wait"] <= row["worst_cash_wait"] and
            (other["avg_borrow"] > row["avg_borrow"] or
             other["mean_borrower_apr"] < row["mean_borrower_apr"] or
             other["worst_senior_loss"] < row["worst_senior_loss"] or
             other["worst_cash_wait"] < row["worst_cash_wait"])
            for other in summaries)
    write_csv("summary.csv", summaries)
    (OUT/"manifest.json").write_text(json.dumps(dict(
        base_params=asdict(Params()),scenarios=[asdict(s) for s in SCENARIOS],
        configurations=len(configurations),simulation_runs=len(rows),
        model="synthetic; not calibrated to live borrower/LP behavior",
        insurance="2% external budget, 20% event draw cap; one loss event",
        revenue="net swap/borrow LP revenue 50/50; former liquidation insurance fee 100% junior",
        liquidations="exogenous loss shock, plus a separate solvent batch paying a 2% fee on 10% of principal",
        seed="0.1% of initial creator liquidity; seed income compounds; user income stays separate",
        zero_junior="borrowing allowed subject to cash admission"),indent=2)+"\n")
    print(f"Saved {len(rows)} runs; {sum(x['pareto'] for x in summaries)} nondominated configurations",flush=True)


if __name__ == "__main__":
    screen()
