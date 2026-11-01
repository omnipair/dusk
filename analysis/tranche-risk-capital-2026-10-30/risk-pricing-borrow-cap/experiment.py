"""Risk-only price with a separate utilization admission limit.

Research-only. Reuses synthetic loan accounting, not Dusk runtime execution.
Tranche notices mature into senior claims without cash; LP cash exits are separate.
"""
import csv,json,hashlib,sys
from dataclasses import replace
from math import exp,log
from pathlib import Path

ROOT=Path(__file__).resolve().parent
sys.path.insert(0,str(ROOT.parent/"hybrid"))
import model as m
sys.path.insert(0,str(ROOT.parent/"feedback"))
from compare import curve,util_error

POLICIES=("utilization","utilization_cap","risk","risk_cap")
SETTINGS={"base":{},"inelastic":{"elasticity":0},"elastic":{"elasticity":2},
          "slow":{"response_days":30},"no_repayment":{"voluntary_repayment":False,"elasticity":0},
          "cash_paid":{"cash_interest_fraction":1}}
SCENARIOS={
    "calm":{},
    "demand_growth":{"growth_day":20,"demand":2},
    "senior_cash_exit":{"senior_day":20},
    "junior_to_senior":{"notice_day":20},
    "senior_then_conversion":{"senior_day":20,"notice_day":21},
    "growth_conversion_crash":{"growth_day":20,"demand":2,"notice_day":25,"crash_day":40,"price":.6},
}
CAP=.80
TARGET_COVERAGE=.25


def borrow_capacity(debt,cash,cap):
    if cap is None:return max(0,cash)
    # During loan origination: debt+=x, cash-=x, so the denominator is unchanged.
    return max(0,min(cash,cap*(debt+cash)-debt))


def risk_step(rate,committed,debt,dt):
    ratio=committed/debt if debt>m.EPS else 1e6
    err=max(-1,min(2,1-ratio/TARGET_COVERAGE))
    if abs(err)<=.05:err=0
    return min(1,max(.01,rate*exp(log(2)*err*dt/7)))


def convert_ready(st,day):
    keep=[]
    for req in st.exits:
        if req.ready>day+m.EPS:
            keep.append(req);continue
        # The prior loss/interest checkpoints determine current NAV.
        amount=min(st.junior,req.shares*st.nav)
        st.junior-=amount
        st.senior+=amount
        st.shares=max(0,st.shares-req.shares)
        st.totals["converted_value"]+=amount
        st.totals["converted_shares"]+=req.shares
    st.exits=keep


def adjust_debt(st,growth,p,cap):
    apr=p.funding+st.premium
    desired_debt=min(12_000_000,p.initial_debt*growth*(apr/.08)**(-p.elasticity))
    desired=(desired_debt-st.debt)*(1-exp(-p.dt/p.response_days))
    if desired<0 and p.voluntary_repayment and st.debt>m.EPS:
        f=min(1,-desired/st.debt)
        for loan in st.loans:
            part=m.Loan(loan.principal*f,loan.collateral*f,loan.funding_due*f,loan.premium_due*f)
            loan.principal*=1-f;loan.collateral*=1-f
            loan.funding_due*=1-f;loan.premium_due*=1-f
            m.collect(st,part,part.due)
        return False,0
    if desired>0:
        cash_only=min(desired,st.cash)
        amount=min(desired,borrow_capacity(st.debt,st.cash,cap))
        clipped=cash_only-amount
        if amount>m.EPS:
            st.cash-=amount
            st.loans.append(m.Loan(amount,amount/(p.origination_ltv*st.price)))
            st.totals["originated"]+=amount
            if cap is not None:
                assert st.debt/(st.debt+st.cash)<=cap+1e-10
        return clipped>m.EPS,clipped
    return False,0


def start(updates):
    p=replace(m.Params(),base_units=0,reserve_fraction=0,initial_debt=8_000_000,
              admission=False,exit_policy="fixed",dt=.25,**updates)
    st=m.initialize(p)
    for k in ("converted_value","converted_shares","senior_withdrawn","cap_clipped_attempts"):
        st.totals[k]=0
    return p,st


def run(policy,scenario,updates):
    p,st=start(updates)
    c=SCENARIOS[scenario]
    cap=CAP if policy.endswith("_cap") else None
    pending_senior=requested_senior=0
    risk_rate=.08
    anchor=.08/curve(util_error(.8))
    rows=[]

    def pay_senior():
        nonlocal pending_senior
        paid=min(pending_senior,st.cash,st.senior)
        st.cash-=paid;st.senior-=paid;pending_senior-=paid
        st.totals["withdrawn"]+=paid;st.totals["senior_withdrawn"]+=paid

    def snapshot(day,binding=False):
        row=m.snapshot(st,day,p)
        # No stress weights enter pricing or admission in this experiment.
        row.pop("forward_coverage");row.pop("actual_coverage")
        row.update(committed_junior=max(0,st.junior-st.pending*st.nav),
                   utilization=st.debt/(st.debt+st.cash) if st.debt+st.cash else 0,
                   cap_binding=int(binding),senior_pending=pending_senior,
                   senior_requested=requested_senior)
        return row

    rows.append(snapshot(0))
    for n in range(1,round(p.days/p.dt)+1):
        day=n*p.dt
        if abs(day-c.get("crash_day",-1))<m.EPS:m.mark(st,c["price"])
        m.liquidate(st,p,.10)
        if abs(day-c.get("senior_day",-1))<m.EPS:
            requested_senior=pending_senior=2_000_000
        if abs(day-c.get("notice_day",-1))<m.EPS:m.request_exit(st,day,.5,p)
        convert_ready(st,day)
        pay_senior()
        committed=max(0,st.junior-st.pending*st.nav)
        risk_rate=risk_step(risk_rate,committed,st.debt,p.dt)
        if policy.startswith("utilization"):
            u=st.debt/(st.debt+st.cash) if st.debt+st.cash else 0
            err=util_error(u)
            anchor=min(1,max(.001,anchor*exp(20*err*p.dt/365)))
            apr=min(1,anchor*curve(err))
        else:apr=risk_rate
        # Common collected-interest split, so only price/admission changes.
        p=replace(p,funding=apr/2);st.premium=apr/2
        m.accrue(st,p)
        growth=c.get("demand",1) if day>=c.get("growth_day",1e6) else 1
        binding,clipped=adjust_debt(st,growth,p,cap)
        st.totals["cap_clipped_attempts"]+=clipped
        m.liquidate(st,p,.10,rate_trigger=True)
        pay_senior()
        rows.append(snapshot(day,binding))
    return rows


def write(path,rows):
    with path.open("w",newline="") as f:
        w=csv.DictWriter(f,fieldnames=list(rows[0]));w.writeheader();w.writerows(rows)


def main():
    out=ROOT/"results";out.mkdir(exist_ok=True)
    traces=out/"traces";traces.mkdir(exist_ok=True)
    summaries=[]
    for setting,updates in SETTINGS.items():
        for policy in POLICIES:
            for scenario in SCENARIOS:
                rows=run(policy,scenario,updates)
                last=rows[-1]
                summaries.append(dict(setting=setting,policy=policy,scenario=scenario,
                    avg_debt=sum(r["debt"] for r in rows[1:])/360,
                    quoted_apr=sum(r["debt"]*r["apr"] for r in rows[1:])/sum(r["debt"] for r in rows[1:]),
                    peak_apr=max(r["apr"] for r in rows),
                    cap_binding_days=sum(r["cap_binding"] for r in rows[1:])*.25,
                    days_above_80pct=sum(r["utilization"]>.8+1e-9 for r in rows[1:])*.25,
                    senior_withdrawn=last["senior_withdrawn"],senior_pending=last["senior_pending"],
                    junior_converted=last["converted_value"],conversion_pending=last["pending_value"],
                    senior_loss=last["senior_loss"],junior_loss=last["junior_loss"],
                    junior_income=last["junior_income"],senior_income=last["senior_income"],
                    interest_collected=last["interest_collected"],ending_cash=last["cash"],
                    ending_debt=last["debt"]))
                if setting=="base":write(traces/f"{policy}_{scenario}.csv",rows)
        print("finished",setting,flush=True)
    write(out/"summary.csv",summaries)
    (out/"manifest.json").write_text(json.dumps({"runs":len(summaries),"cap":CAP,
        "target_junior_debt":TARGET_COVERAGE,"settings":SETTINGS,"scenarios":SCENARIOS,
        "synthetic":True,"native_dusk_execution":False,
        "source_hashes":{str(p.relative_to(ROOT.parent)):hashlib.sha256(p.read_bytes()).hexdigest()
            for p in (ROOT/"experiment.py",ROOT.parent/"hybrid/model.py",ROOT.parent/"feedback/compare.py")}},indent=2)+"\n")


if __name__=="__main__":main()
