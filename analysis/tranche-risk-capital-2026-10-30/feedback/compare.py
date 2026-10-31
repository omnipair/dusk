"""Matched-state controller response probe, not a market-performance simulation.

Hold each shocked book constant for seven days: interest cash paid and distributed,
no further deposits, exits, loan growth, credit events or behavioral response.
The utilization arm uses Dusk's curve shape and continuous-time anchor drift;
it is not execution of the program's bounded fixed-point update.
"""
from dataclasses import dataclass, asdict
from math import exp, log
from pathlib import Path
import csv
import hashlib
import json


@dataclass(frozen=True)
class Book:
    name: str
    cash: float
    debt: float
    senior: float
    junior: float
    pending_junior: float = 0
    loss_weight: float = 1

    @property
    def assets(self): return self.cash+self.debt
    @property
    def committed(self): return self.junior-self.pending_junior


BOOKS=(
    Book("unchanged",20,80,80,20),
    Book("senior_cash_exit_20",0,80,60,20),
    Book("junior_switches_10_to_senior",20,80,90,10),
    Book("borrowers_repay_40",60,40,80,20),
    Book("new_loans_10",10,90,80,20),
    Book("fresh_junior_cash_20",40,80,80,40),
    Book("senior_exit_then_junior_notice_10",0,80,60,20,10),
    Book("observed_loss_weight_doubles",20,80,80,20,0,2),
)

TARGET_U=.70
STEEPNESS=4
ADAPTATION=20
INITIAL_RATE=.08
TARGET_SHARE=.20
TARGET_COVERAGE=.25
HALF_LIFE=7


def util_error(u):
    return (u-TARGET_U)/(1-TARGET_U) if u>=TARGET_U else (u-TARGET_U)/TARGET_U


def curve(e):
    return 1+(STEEPNESS-1)*e if e>=0 else 1+(1-1/STEEPNESS)*e


# Start every controller at 8% on the SAME original book (80% utilization).
ANCHOR=INITIAL_RATE/curve(util_error(.8))


def utilization_rate(b,days):
    e=util_error(b.debt/b.assets)
    return min(1,ANCHOR*exp(ADAPTATION*e*days/365)*curve(e))


def coverage_rate(ratio,target,days):
    error=max(-1,min(2,1-ratio/target))
    if abs(error)<=.05: error=0
    return min(1,max(.01,INITIAL_RATE*exp(log(2)*error*days/HALF_LIFE)))


def evaluate(b,days):
    share=b.junior/b.assets
    committed_share=b.committed/b.assets
    exposure=b.committed/(b.debt*b.loss_weight)
    u=utilization_rate(b,days)
    credit=coverage_rate(exposure,TARGET_COVERAGE,days)
    return dict(**asdict(b),days=days,utilization=b.debt/b.assets,
        raw_junior_share=share,committed_junior_share=committed_share,
        junior_per_weighted_debt=exposure,
        utilization_apr=u,
        raw_participation_apr=coverage_rate(share,TARGET_SHARE,days),
        committed_participation_apr=coverage_rate(committed_share,TARGET_SHARE,days),
        coverage_apr=credit,coverage_with_liquidity_floor_apr=max(u,credit))


def main():
    rows=[evaluate(b,d) for b in BOOKS for d in (0,7)]
    for b in BOOKS:
        assert b.assets==b.senior+b.junior
        assert 0<=b.pending_junior<=b.junior
        # Exact identity in a one-asset book; loss weighting explicitly changes it.
        assert abs((b.committed/b.assets)/(b.debt/b.assets)-b.committed/b.debt)<1e-12
    out=Path(__file__).resolve().parent
    with (out/"controller_responses.csv").open("w",newline="") as f:
        w=csv.DictWriter(f,fieldnames=list(rows[0]));w.writeheader();w.writerows(rows)
    lookup={(r["name"],r["days"]):r for r in rows}
    s=lookup[("senior_cash_exit_20",7)]
    assert s["raw_participation_apr"]<INITIAL_RATE
    assert s["coverage_apr"]==INITIAL_RATE
    assert s["utilization_apr"]>INITIAL_RATE
    assert s["coverage_with_liquidity_floor_apr"]==s["utilization_apr"]
    n=lookup[("senior_exit_then_junior_notice_10",7)]
    assert n["raw_participation_apr"]<INITIAL_RATE<n["committed_participation_apr"]
    assert n["coverage_apr"]>INITIAL_RATE
    p=lookup[("borrowers_repay_40",7)]
    assert p["raw_participation_apr"]==INITIAL_RATE>p["coverage_apr"]
    info={"matched_state_rows":len(rows),"accounting_and_direction_checks":"passed",
          "initial_apr":INITIAL_RATE,"utilization_target":TARGET_U,"curve_steepness":STEEPNESS,
          "utilization_adaptation_per_year":ADAPTATION,"coverage_target":TARGET_COVERAGE,
          "junior_share_target":TARGET_SHARE,"risk_half_life_days":HALF_LIFE,
          "synthetic":True,"performance_claim":False,
          "source_sha256":hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    (out/"verification.json").write_text(json.dumps(info,indent=2)+"\n")
    for r in rows:
        if r["days"]==7:
            print(r["name"],{k:round(100*r[k],2) for k in r if k.endswith("apr")})


if __name__=="__main__":main()
