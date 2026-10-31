"""Independent reconciliation of exported traces. Does not import model.py."""
import csv
import json
from pathlib import Path

ROOT=Path(__file__).resolve().parent
checks=0
maximum=0


def check(a,b):
    global checks, maximum
    residual=abs(a-b)
    maximum=max(maximum,residual)
    checks+=1
    assert residual < .001, (checks,a,b,residual)


for path in sorted((ROOT/"results/traces").glob("*.csv")):
    rows=[{k:float(v) for k,v in r.items()} for r in csv.DictReader(path.open())]
    initial=rows[0]
    capital=initial["senior"]+initial["junior"]
    for r in rows:
        assets=r["cash"]+r["base"]*r["price"]+r["principal"]
        check(assets,r["active"])
        check(assets+r["reserve"],r["senior"]+r["junior"])
        check(assets+r["reserve"]+r["withdrawn"]+r["inventory_loss"]+
              r["principal_loss"],capital+r["new_capital"])
        check(r["principal_loss"],r["junior_loss"]+r["senior_loss"])
        check(r["interest_collected"],r["junior_income"]+r["senior_income"])
        check(r["interest_accrued"],r["interest_collected"]+r["interest_written_off"]+r["unpaid"])
        check(r["debt"],r["principal"]+r["unpaid"])
        check(initial["principal"]+r["originated"],
              r["principal"]+r["principal_recovered"]+r["principal_loss"])
        check(r["shares"],initial["shares"]-r["exit_paid_shares"]
              if r["junior"]>0 else 0)
        if r["exit_requested"]>0:
            check(1_000_000,r["pending_shares"]+r["exit_paid_shares"]+r["exit_lost_shares"])
        assert r["reserve"]<=r["junior"]+.001
        assert min(r["cash"],r["reserve"],r["junior"],r["senior"])>=-.001

result={"identities_checked":checks,"max_residual_dollars_or_shares":maximum,
        "base_traces":len(list((ROOT/"results/traces").glob("*.csv"))),"passed":True}
(ROOT/"results/verification.json").write_text(json.dumps(result,indent=2)+"\n")
print(json.dumps(result,indent=2))
