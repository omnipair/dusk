from dataclasses import replace
from itertools import product
import json
from model import Params, Scenario, Simulation, SCENARIOS
from run import OUT, write_csv


PROFILES={
    "balanced_slow":Params(target=.05,half_days=14,cap=.5,borrow_limit=.8),
    "balanced_fast":Params(target=.05,half_days=7,cap=.5,borrow_limit=.8),
    "lower_target":Params(target=.02,half_days=14,cap=.5,borrow_limit=.8),
    "higher_target":Params(target=.10,half_days=14,cap=.5,borrow_limit=.8),
}


def run():
    rows=[]
    for name,p in PROFILES.items():
        for elasticity,hurdle,arrival in product((0.,.5,1.5),(.1,.15,.3),(7,30)):
            for bootstrap in (False,True):
                sc=Scenario("behavior",elasticity=elasticity,junior_hurdle=hurdle,
                    junior_days=arrival,junior_fraction=.001 if bootstrap else .2,senior_exit=.25)
                rows.append(dict(profile=name,elasticity=elasticity,hurdle=hurdle,
                    junior_days=arrival,bootstrap=bootstrap,**Simulation(p,sc).run()))
        for shock_day,initial in product((5,30,60),(.001,.2)):
            sc=Scenario("timing_stress",junior_fraction=initial,shock_loss_fraction=.30,
                        base_mark=.65,shock_day=shock_day)
            rows.append(dict(profile=name,elasticity=1,hurdle=.15,junior_days=7,
                bootstrap=initial==.001,**Simulation(p,sc).run()))
    write_csv("behavior_sensitivity.csv",rows)

    rows=[]
    base=PROFILES["balanced_slow"]
    for floor,start,band in product((.005,.01,.02),(.02,.04,.08),(.05,.10,.20)):
        p=replace(base,floor=floor,initial_rate=start,band=band)
        for sc in SCENARIOS:
            rows.append(dict(floor=floor,initial_rate=start,band=band,**Simulation(p,sc).run()))
    write_csv("calibration_sensitivity.csv",rows)

    rows=[]
    for name,p in PROFILES.items():
        for sc in SCENARIOS:
            for dt in (1.,.25):
                rows.append(dict(profile=name,dt=dt,**Simulation(replace(p,dt=dt),sc).run()))
    write_csv("time_step.csv",rows)

    rows=[]
    for signal in ("combined","coverage","utilization"):
        p=replace(base,signal=signal,target=.25 if signal=="coverage" else .05)
        for sc in SCENARIOS:
            m=Simulation(p,sc)
            rows.append(dict(signal=signal,**m.run()))
            if signal=="combined":
                write_csv(f"trace_{sc.name}.csv",m.history)
    write_csv("signal_comparison.csv",rows)

    rows=[]
    for fee,destination in ((0,"seed"),(5,"seed"),(5,"remaining")):
        for sc in SCENARIOS:
            rows.append(dict(fee_bps=fee,destination=destination,
                **Simulation(replace(base,queue_fee_bps=fee,fee_destination=destination),sc).run()))
    write_csv("queue_fees.csv",rows)

    rows=[]
    extra=[Scenario("seed_only_inelastic",junior_fraction=.001,junior_enabled=False,elasticity=0),
           Scenario("zero_junior_no_swaps",junior_fraction=0,junior_enabled=False,swap_fee_apr=0),
           Scenario("no_debt_bootstrap",junior_fraction=.001,utilization=(0,0)),
           Scenario("thin_seed",junior_fraction=.0001,junior_enabled=False)]
    for cap,lp_signal,share in product((.5,1.,2.),("forecast","collected"),(.8,.9,1.)):
        for sc in [SCENARIOS[1],SCENARIOS[2],SCENARIOS[8],*extra]:
            rows.append(dict(cap=cap,lp_signal=lp_signal,lp_interest_fraction=share,
                **Simulation(replace(base,cap=cap,lp_revenue_signal=lp_signal,lp_interest_fraction=share),sc).run()))
    write_csv("bootstrap_and_collection.csv",rows)
    (OUT/"refinement.json").write_text(json.dumps({"profiles":{k:vars(v) for k,v in PROFILES.items()},
        "run_counts":{"behavior":168,"calibration":270,"time_step":80,"signal_comparison":30,
                      "queue_fees":30,"bootstrap_collection":126}},indent=2)+"\n")
    print("Refinement and behavioral sensitivity runs saved",flush=True)


if __name__=="__main__":
    run()
