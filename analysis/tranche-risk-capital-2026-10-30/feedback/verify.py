"""Independent balance-sheet checks on exported dynamic traces."""
import csv,json
from pathlib import Path
ROOT=Path(__file__).resolve().parent/"dynamic_results"
checks=0
maximum=0
for path in (ROOT/"traces").glob("*.csv"):
    for raw in csv.DictReader(path.open()):
        r={k:float(v) for k,v in raw.items()}
        pairs=[(r["cash"]+r["principal"],r["senior"]+r["junior"]),
               (r["senior"]+r["junior"]+r["withdrawn"]+r["principal_loss"],10_000_000),
               (r["interest_accrued"],r["interest_collected"]+r["interest_written_off"]+r["unpaid"]),
               (r["principal_loss"],r["junior_loss"]+r["senior_loss"]),
               (r["interest_collected"],r["junior_income"]+r["senior_income"]),
               (8_000_000+r["originated"],r["principal"]+r["principal_recovered"]+r["principal_loss"]),
               (r["senior_requested"],r["senior_withdrawn"]+r["senior_pending"])]
        for a,b in pairs:
            err=abs(a-b)
            assert err<.001,(path,r["day"],a,b)
            checks+=1;maximum=max(maximum,err)
        assert min(r["cash"],r["junior"],r["senior"])>=-.001
report={"identities_checked":checks,"max_residual":maximum,"passed":True,
        "method":"Independent exported-trace identities; does not import simulator."}
(ROOT/"verification.json").write_text(json.dumps(report,indent=2)+"\n")
print(json.dumps(report,indent=2))
