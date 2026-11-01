"""Independent balance-sheet reconciliation; no simulator imports."""
import csv,json
from pathlib import Path
ROOT=Path(__file__).resolve().parent/"results"
checks=0;maximum=0
for path in (ROOT/"traces").glob("*.csv"):
    for raw in csv.DictReader(path.open()):
        r={k:float(v) for k,v in raw.items()}
        pairs=[(r["cash"]+r["principal"],r["senior"]+r["junior"]),
               (r["senior"]+r["junior"]+r["withdrawn"]+r["principal_loss"],10_000_000),
               (r["principal_loss"],r["junior_loss"]+r["senior_loss"]),
               (r["interest_accrued"],r["unpaid"]+r["interest_collected"]+r["interest_written_off"]),
               (r["interest_collected"],r["junior_income"]+r["senior_income"]),
               (8_000_000+r["originated"],r["principal"]+r["principal_recovered"]+r["principal_loss"]),
               (r["senior_requested"],r["senior_withdrawn"]+r["senior_pending"])]
        for a,b in pairs:
            err=abs(a-b);checks+=1;maximum=max(maximum,err)
            assert err<.001,(path,r["day"],a,b)
        assert min(r["cash"],r["junior"],r["senior"])>=-.001
        if r["exit_requested"]>0:
            assert abs(r["pending_shares"]+r["converted_shares"]+r["exit_lost_shares"]-1_000_000)<.001
            checks+=1
result={"passed":True,"independent_identities":checks,"maximum_residual":maximum}
(ROOT/"verification.json").write_text(json.dumps(result,indent=2)+"\n")
print(json.dumps(result,indent=2))
