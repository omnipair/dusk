import csv
import hashlib
import json
import os
from pathlib import Path
os.environ.setdefault("MPLCONFIGDIR","/private/tmp/dusk-tranche-matplotlib")
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

ROOT=Path(__file__).resolve().parent
OUT=ROOT/"results"
rows=list(csv.DictReader((OUT/"summary.csv").open()))


def get(setting,scenario,policy,admission=1,reserve=.05):
    return next(r for r in rows if r["setting"]==setting and r["scenario"]==scenario
                and r["exit_policy"]==policy and int(r["admission"])==admission
                and float(r["reserve_fraction"])==reserve)


def trace(policy,admission,scenario):
    path=OUT/"traces"/f"r0.05_{policy}_a{admission}_{scenario}.csv"
    return [{k:float(v) for k,v in r.items()} for r in csv.DictReader(path.open())]


def main():
    plt.rcParams.update({"font.family":"DejaVu Sans","font.size":10,
        "axes.spines.top":False,"axes.spines.right":False,"axes.titlesize":12,
        "axes.titleweight":"bold","axes.grid":True,"grid.alpha":.18,
        "axes.axisbelow":True,"text.parse_math":False})
    fig,axes=plt.subplots(2,2,figsize=(13,9))
    colors={"fixed":"#cd6a40","coverage":"#297e93"}
    for policy in colors:
        tr=trace(policy,1,"exit_only")
        axes[0,0].plot([r["day"] for r in tr],[r["withdrawn"]/1e6 for r in tr],
                        color=colors[policy],lw=2,label="Fixed date" if policy=="fixed" else "Coverage queue")
    axes[0,0].axvline(37,color="#777",ls="--",lw=1)
    axes[0,0].set(title="Exit payments when borrowers respond",xlabel="Day",ylabel="Cumulative junior cash paid ($m)",ylim=(0,1.1))
    axes[0,0].legend(frameon=False,loc="lower right")
    ax=axes[0,1]
    for i,policy in enumerate(colors):
        r=get("no_repayment","exit_only",policy)
        paid=float(r["withdrawn"])/1e6
        pending=float(r["pending_value"])/1e6
        ax.bar(i,paid,color=colors[policy],width=.5)
        ax.bar(i,pending,bottom=paid,color="#dce1e5",width=.5,hatch="///",edgecolor="#8a929b")
        ax.text(i,paid/2,f"Paid\n${paid:.3f}m",ha="center",va="center",color="white",fontweight="bold")
        if pending: ax.text(i,paid+pending/2,f"Still queued\n${pending:.3f}m",ha="center",va="center")
    ax.set(title="No replacement capital or voluntary repayment",xticks=[0,1],xticklabels=["Fixed date","Coverage queue"],
           ylabel="$1m exit request at day 90",ylim=(0,1.12))
    ax=axes[1,0]
    labels=["Exit + crash","Growth + crash","Exit + larger crash"]
    for i,policy in enumerate(colors):
        values=[float(get("base",sc,policy)["senior_loss"])/1e6 for sc in ("exit_crash","growth_crash","exit_tail")]
        xs=[x+(i-.5)*.33 for x in range(3)]
        ax.bar(xs,values,width=.3,color=colors[policy])
        for x,v in zip(xs,values): ax.text(x,v+.025,f"{v:.2f}",ha="center",fontsize=9)
    ax.set(title="Senior credit loss; new-loan admission checked",xticks=range(3),xticklabels=labels,
           ylabel="Funded principal loss ($m)",ylim=(0,1.2))
    ax=axes[1,1]
    for admission,color,label in ((0,"#8270ac","Price only"),(1,"#297e93","Price + admission check")):
        tr=trace("coverage",admission,"growth_crash")
        ax.plot([r["day"] for r in tr],[r["debt"]/1e6 for r in tr],color=color,lw=2,label=label)
    ax.axvline(40,color="#777",ls="--",lw=1)
    ax.set(title="Price response alone allows coverage overshoot",xlabel="Day",ylabel="Outstanding debt ($m)",ylim=(0,6))
    ax.legend(frameon=False,loc="lower right")
    fig.suptitle("Junior coverage, exit timing, and usable capital",x=.07,ha="left",fontsize=19,fontweight="bold")
    fig.text(.07,.925,"Synthetic experiment • $10m equity, $2m junior including $0.5m reserve • no external underwriting inflows",fontsize=10)
    fig.text(.07,.025,"Seven-day exit notice; 45% remaining-price crash, 30% larger crash. Ordinary inventory losses excluded from credit-loss bars.\n"
             "Coverage is conditional on declared stresses; it is not a guarantee. Both exit policies still require actual quote cash.",fontsize=9,color="#555")
    fig.subplots_adjust(left=.07,right=.97,top=.86,bottom=.12,hspace=.38,wspace=.27)
    fig.savefig(OUT/"hybrid-tradeoffs.png",dpi=180)
    plt.close(fig)
    comparisons=[]
    for r in rows:
        if r["exit_policy"]!="coverage": continue
        fixed=get(r["setting"],r["scenario"],"fixed",int(r["admission"]),float(r["reserve_fraction"]))
        comparisons.append({k:r[k] for k in ("setting","scenario","reserve_fraction","admission")} |
            {"queue_minus_fixed_senior_loss":float(r["senior_loss"])-float(fixed["senior_loss"]),
             "queue_minus_fixed_cash_paid":float(r["withdrawn"])-float(fixed["withdrawn"])})
    with (OUT/"exit_comparisons.csv").open("w",newline="") as f:
        w=csv.DictWriter(f,fieldnames=list(comparisons[0])); w.writeheader(); w.writerows(comparisons)
    findings={"synthetic":True,"no_repayment_exit_only":{
        p:{k:float(get("no_repayment","exit_only",p)[k]) for k in
           ("withdrawn","pending_value","pending_shares","peak_apr","interest_collected")}
        for p in colors},"base_exit_crash":{
        p:{k:float(get("base","exit_crash",p)[k]) for k in
           ("withdrawn","senior_loss","junior_loss","inventory_loss")}
        for p in colors}}
    (OUT/"findings.json").write_text(json.dumps(findings,indent=2)+"\n")
    print(json.dumps(findings,indent=2))


if __name__=="__main__": main()
