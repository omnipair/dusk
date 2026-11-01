import csv
import hashlib
import json
import subprocess
from dataclasses import asdict, replace
from pathlib import Path
from model import Params, SCENARIOS, run, coverage, initialize

ROOT = Path(__file__).resolve().parent
OUT = ROOT/"results"
OUT.mkdir(exist_ok=True)
(OUT/"traces").mkdir(exist_ok=True)

SETTINGS = {
    "base": {},
    "inelastic": {"elasticity":0},
    "elastic": {"elasticity":2},
    "slow_response": {"response_days":30},
    "no_repayment": {"voluntary_repayment":False,"elasticity":0},
    "cash_paid": {"cash_interest_fraction":1},
    "long_notice": {"notice":30},
    "fast_controller": {"half_life":2},
    "slow_controller": {"half_life":30},
    "thin_margin": {"target":1},
    "larger_margin": {"target":1.3},
}


def write_csv(path,rows):
    with path.open("w",newline="") as f:
        writer = csv.DictWriter(f,fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def summarize(rows,p):
    last = rows[-1]
    avg = sum(r["debt"] for r in rows[1:])/len(rows[1:])
    weighted_apr = sum(r["apr"]*r["debt"] for r in rows[1:])/max(1,sum(r["debt"] for r in rows[1:]))
    # Share-weighted completed exit delay plus current delay of still pending shares.
    paid_shares = last["exit_paid_shares"]
    return dict(avg_debt=avg,avg_debt_capital=avg/p.capital,
                quoted_debt_weighted_apr=weighted_apr,peak_apr=max(r["apr"] for r in rows),
                initial_active=p.capital*(1-p.reserve_fraction),
                completed_exit_delay_days=last["exit_delay_share_days"]/paid_shares if paid_shares else 0,
                pending_share_fraction=last["pending_shares"]/1_000_000,
                min_actual_coverage=min(r["actual_coverage"] for r in rows),
                **{k:last[k] for k in ("debt","unpaid","cash","reserve","junior","senior",
                   "senior_loss","junior_loss","principal_loss","inventory_loss",
                   "withdrawn","exit_requested","pending_value","pending_shares",
                   "overdue_shares","interest_accrued","interest_collected","interest_written_off",
                   "junior_income","senior_income","rate_liquidated_principal","blocked_new_debt")})


def main():
    summaries=[]
    steps=[]
    for setting,updates in SETTINGS.items():
        for reserve in (0,.05,.1,.2):
            for exit_policy in ("fixed","coverage"):
                for admission in (False,True):
                    p=replace(Params(),**updates,reserve_fraction=reserve,
                              exit_policy=exit_policy,admission=admission)
                    for scenario in SCENARIOS:
                        key=dict(setting=setting,reserve_fraction=reserve,
                                 exit_policy=exit_policy,admission=int(admission),scenario=scenario)
                        rows=run(p,scenario)
                        result=summarize(rows,p)
                        summaries.append({**key,**result})
                        if setting=="base":
                            name=f"r{reserve:.2f}_{exit_policy}_a{int(admission)}_{scenario}"
                            write_csv(OUT/"traces"/f"{name}.csv",rows)
                            if reserve==.05:
                                fine=summarize(run(replace(p,dt=.25),scenario),p)
                                steps.append({**key,**{f"delta_{k}":fine[k]-result[k] for k in
                                    ("senior_loss","withdrawn","avg_debt_capital","quoted_debt_weighted_apr")}})
        print(f"finished {setting}",flush=True)
    write_csv(OUT/"summary.csv",summaries)
    write_csv(OUT/"step_sensitivity.csv",steps)
    (OUT/"parameters.json").write_text(json.dumps({"base":asdict(Params()),"settings":SETTINGS,
                                                  "scenarios":SCENARIOS},indent=2)+"\n")
    source_files=list(ROOT.glob("*.py"))+[ROOT/"PREANALYSIS.md"]
    manifest={"runs":len(summaries),"fine_step_runs":len(steps),
              "source_sha256":{p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in source_files},
              "git_head":subprocess.check_output(["git","rev-parse","HEAD"],cwd=ROOT,text=True).strip()}
    (OUT/"manifest.json").write_text(json.dumps(manifest,indent=2)+"\n")
    print(json.dumps(manifest,indent=2))


if __name__=="__main__": main()
