"""Pricing ablation with identical junior backing and exit contracts in all arms.

This tests controllers, NOT the total superiority of tranches over current Dusk
insurance. Reuse the prior synthetic cash-conserving loan/interest engine.
No external deposits or endogenous underwriting demand are fabricated.
"""
import csv
from dataclasses import replace
import hashlib
import json
from math import exp,log
from pathlib import Path
import sys

ROOT=Path(__file__).resolve().parent
sys.path.insert(0,str(ROOT.parent/"hybrid"))
import model as m
from compare import curve,util_error

POLICIES=("utilization","raw_share","committed_share","coverage","coverage_liquidity_floor")
SCENARIOS={
    "calm":{},
    "senior_exit":{"senior_day":20},
    "senior_then_junior_notice":{"senior_day":20,"junior_day":21},
    "senior_junior_crash":{"senior_day":20,"junior_day":21,"crash_day":40,"price":.60},
    "senior_junior_tail":{"senior_day":20,"junior_day":21,"crash_day":40,"price":.45},
    "junior_switch":{"switch_day":20},
}
SETTINGS={"base":{},"inelastic":{"elasticity":0},"elastic":{"elasticity":2},
          "slow":{"response_days":30},"no_repayment":{"voluntary_repayment":False,"elasticity":0},
          "cash_paid":{"cash_interest_fraction":1}}


def risk_step(rate,ratio,target,dt):
    error=max(-1,min(2,1-ratio/target))
    if abs(error)<=.05:error=0
    return min(1,max(.01,rate*exp(log(2)*error*dt/7)))


def run(policy,scenario,updates):
    p=replace(m.Params(),base_units=0,reserve_fraction=0,initial_debt=8_000_000,
              admission=False,exit_policy="fixed",dt=.25,**updates)
    st=m.initialize(p)
    st.totals["senior_withdrawn"]=0
    c=SCENARIOS[scenario]
    remaining_senior=0
    requested_senior=0
    switched=0
    risk_rate=.08
    anchor=.08/curve(util_error(.8))
    rows=[m.snapshot(st,0,p)|{"senior_pending":0,"senior_requested":0,"switched_to_senior":0}]

    def pay_senior():
        nonlocal remaining_senior
        paid=min(remaining_senior,st.cash,st.senior)
        st.cash-=paid;st.senior-=paid;remaining_senior-=paid
        st.totals["withdrawn"]+=paid;st.totals["senior_withdrawn"]+=paid

    for n in range(1,round(p.days/p.dt)+1):
        day=n*p.dt
        if abs(day-c.get("crash_day",-1))<m.EPS:m.mark(st,c["price"])
        m.liquidate(st,p,.1 if c.get("price",1)>=.6 else .2)
        if abs(day-c.get("senior_day",-1))<m.EPS:
            requested_senior=remaining_senior=2_000_000
        if abs(day-c.get("junior_day",-1))<m.EPS:m.request_exit(st,day,.5,p)
        if abs(day-c.get("switch_day",-1))<m.EPS:
            # A previously noticed switch matures. Claim conversion uses no cash.
            amount=min(st.junior,1_000_000)
            shares=amount/st.nav
            st.junior-=amount;st.senior+=amount;st.shares-=shares
            switched+=amount
        pay_senior()
        m.settle_exits(st,day,p)
        u=st.debt/(st.debt+st.cash) if st.debt+st.cash else 0
        e=util_error(u)
        anchor=min(1,max(.001,anchor*exp(20*e*p.dt/365)))
        liquidity_rate=min(1,anchor*curve(e))
        committed=max(0,st.junior-st.pending*st.nav)
        if policy=="raw_share":
            ratio=st.junior/(st.junior+st.senior);target=.2
        elif policy=="committed_share":
            ratio=committed/(st.junior+st.senior);target=.2
        else:
            ratio=committed/st.debt if st.debt>m.EPS else 1e6;target=.25
        risk_rate=risk_step(risk_rate,ratio,target,p.dt)
        apr=(liquidity_rate if policy=="utilization" else
             max(liquidity_rate,risk_rate) if policy=="coverage_liquidity_floor" else risk_rate)
        # IDENTICAL realized revenue allocation in every arm: half is underwriting,
        # half funding distributed across active LP ownership.
        p=replace(p,funding=apr/2)
        st.premium=apr/2
        m.accrue(st,p)
        m.adjust_debt(st,day,1,p)
        m.liquidate(st,p,.1,rate_trigger=True)
        pay_senior()
        m.settle_exits(st,day,p)
        rows.append(m.snapshot(st,day,p)|{"senior_pending":remaining_senior,
                    "senior_requested":requested_senior,"switched_to_senior":switched})
    return rows


def write(path,rows):
    with path.open("w",newline="") as f:
        w=csv.DictWriter(f,fieldnames=list(rows[0]));w.writeheader();w.writerows(rows)


def main():
    out=ROOT/"dynamic_results";out.mkdir(exist_ok=True)
    traces=out/"traces";traces.mkdir(exist_ok=True)
    results=[]
    for setting,updates in SETTINGS.items():
        for policy in POLICIES:
            for scenario in SCENARIOS:
                rows=run(policy,scenario,updates)
                last=rows[-1]
                results.append(dict(setting=setting,policy=policy,scenario=scenario,
                    avg_debt=sum(r["debt"] for r in rows[1:])/360,
                    quoted_debt_weighted_apr=sum(r["debt"]*r["apr"] for r in rows[1:])/sum(r["debt"] for r in rows[1:]),
                    peak_apr=max(r["apr"] for r in rows),
                    senior_withdrawn=last["senior_withdrawn"],senior_pending=last["senior_pending"],
                    junior_cash_paid=last["withdrawn"]-last["senior_withdrawn"],
                    junior_pending_value=last["pending_value"],junior_pending_shares=last["pending_shares"],
                    senior_loss=last["senior_loss"],junior_loss=last["junior_loss"],
                    junior_income=last["junior_income"],senior_income=last["senior_income"],
                    interest_collected=last["interest_collected"],unpaid=last["unpaid"],
                    ending_cash=last["cash"],ending_debt=last["debt"]))
                if setting=="base":write(traces/f"{policy}_{scenario}.csv",rows)
        print("finished",setting,flush=True)
    write(out/"summary.csv",results)
    manifest={"runs":len(results),"settings":SETTINGS,"scenarios":SCENARIOS,
              "matched_terms":"All arms have identical productive junior, waterfall, notice and income split.",
              "not_current_dusk_comparator":True,"synthetic":True,
              "source_sha256":{str(x.relative_to(ROOT.parent)):hashlib.sha256(x.read_bytes()).hexdigest()
                  for x in (ROOT/"dynamics.py",ROOT/"compare.py",ROOT.parent/"hybrid/model.py")}}
    (out/"manifest.json").write_text(json.dumps(manifest,indent=2)+"\n")


if __name__=="__main__":main()
