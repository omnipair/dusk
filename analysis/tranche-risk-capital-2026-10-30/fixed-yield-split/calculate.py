"""Exact revenue-split arithmetic; no behavioral or loss forecast."""
from fractions import Fraction as F
from pathlib import Path
import json


def yields(senior,junior,interest):
    assert senior>0 and junior>0
    j=interest/2
    s=interest-j
    assert j+s==interest
    yj=j/junior
    ys=s/senior
    assert yj/ys==senior/junior
    return {"senior":float(senior),"junior":float(junior),
            "junior_interest":float(j),"senior_interest":float(s),
            "junior_interest_apr":float(yj),"senior_interest_apr":float(ys)}


def main():
    interest=F(80)*F(8,100)
    rows=[yields(F(100-j),F(j),interest) for j in (5,10,20,30,40,50,60)]
    before={"J":F(20),"D":F(80),"C":F(20)}
    after={"J":F(20),"D":F(80),"C":F(10)}
    ratios=[]
    for name,b in (("before",before),("senior_withdraws_10",after)):
        total=b["D"]+b["C"]
        q=b["J"]/total
        u=b["D"]/total
        assert q/u==b["J"]/b["D"]
        ratios.append({"state":name,"junior_share":float(q),"utilization":float(u),
                       "share_over_utilization":float(q/u),
                       "amount_over_utilization":float(b["J"]/u)})
    output={"synthetic":True,"capital":100,"debt":80,"borrow_apr":.08,
            "net_annual_distributable_interest":float(interest),
            "fees_assumed":0,"losses_assumed_for_yield_quote":0,
            "interest_split":rows,"ratio_identity_examples":ratios,
            "verification":"Exact rational revenue conservation and ratio identities passed."}
    path=Path(__file__).resolve().parent/"calculations.json"
    path.write_text(json.dumps(output,indent=2)+"\n")
    print(json.dumps(output,indent=2))


if __name__=="__main__":main()
