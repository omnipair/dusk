"""Static directional debt/cash checks, independent of the dynamic model."""
import csv
from pathlib import Path

G=10_000_000.0
J=2_000_000.0
LTV=.70
LIQ=.85
MARGIN=1.10
# Opposing directional shocks and an explicit common recovery failure scenario.
STRESSES=(("base_down",.45,.20), ("base_up",1/.45,.20),
          ("common_recovery_failure",1.0,.60))


def evaluate(debt,base_debt_fraction,reserve,reserve_base_fraction):
    rb=reserve*reserve_base_fraction
    rq=reserve-rb
    ab=G/2-rb
    aq=G/2-rq
    db=debt*base_debt_fraction
    dq=debt-db
    if db>ab+1e-6 or dq>aq+1e-6:
        return -1,"cash"
    q=(J-reserve)/(G-reserve)
    values=[]
    for name,price,discount in STRESSES:
        # Liquidator execution failure can be recognized as bad debt even when
        # oracle prices do not move; the common-failure arm is an explicit
        # default stress, not a regular LTV-triggered liquidation prediction.
        base_trigger=LTV*price>=LIQ or name=="common_recovery_failure"
        quote_trigger=LTV/price>=LIQ or name=="common_recovery_failure"
        base_loss=max(0,db-db/LTV*(1-discount)/price) if base_trigger else 0
        quote_loss=max(0,dq-dq/LTV*price*(1-discount)) if quote_trigger else 0
        loss=base_loss*price+quote_loss
        junior=q*(ab*price+aq)+rb*price+rq
        ratio=junior/loss if loss else 1e6
        values.append((ratio,name))
    return min(values)


def run():
    rows=[]
    for reserve_fraction in (0,.05,.1,.2):
        reserve=G*reserve_fraction
        for debt_mix in (0,.5,1):
            for reserve_mix in (0,.5,1):
                lo,hi=0,G
                for _ in range(55):
                    mid=(lo+hi)/2
                    if evaluate(mid,debt_mix,reserve,reserve_mix)[0]>=MARGIN:
                        lo=mid
                    else: hi=mid
                ratio,binding=evaluate(lo,debt_mix,reserve,reserve_mix)
                rb=reserve*reserve_mix
                rq=reserve-rb
                base_cash=G/2-rb-lo*debt_mix
                quote_cash=G/2-rq-lo*(1-debt_mix)
                if (debt_mix > 0 and base_cash < .01) or (debt_mix < 1 and quote_cash < .01):
                    binding="available_debt_token_cash"
                rows.append(dict(reserve_fraction=reserve_fraction,
                    base_debt_fraction=debt_mix,reserve_base_fraction=reserve_mix,
                    active_backing=G-reserve,max_debt=lo,coverage_at_max=ratio,
                    binding_stress=binding,base_cash=base_cash,quote_cash=quote_cash))
    out=Path(__file__).resolve().parent/"results/two_asset_capacity.csv"
    with out.open("w",newline="") as f:
        w=csv.DictWriter(f,fieldnames=list(rows[0]))
        w.writeheader(); w.writerows(rows)
    print(f"saved {len(rows)} static directional cases")


if __name__=="__main__": run()
