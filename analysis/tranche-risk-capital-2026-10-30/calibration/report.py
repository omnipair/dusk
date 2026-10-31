"""Render the source-backed review artifact; all figures are synthetic results."""
from pathlib import Path
from fractions import Fraction as F
from statistics import mean
from dataclasses import replace
import base64
import csv
import json
import os

ROOT=Path(__file__).parent
OUT=ROOT/"results"
BRAND=ROOT.parents[2]/"assets"/"brand"
os.environ.setdefault("MPLCONFIGDIR","/private/tmp/dusk-tranche-calibration-mpl")
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.ticker import PercentFormatter, FuncFormatter


def read(name):
    return list(csv.DictReader((OUT/name).open()))


def stats(rows):
    return dict(borrow=mean(float(x["avg_borrow"]) for x in rows),
        apr=mean(float(x["borrower_apr"]) for x in rows),
        senior_loss=max(float(x["senior_loss"]) for x in rows),
        wait=max(float(x["average_wait"]) for x in rows),
        turns=mean(float(x["rate_turns"]) for x in rows))


def pct(x):return f"{100*x:.2f}%"
def money(x):return f"${x:,.0f}"


def main():
    profile_names={"lower_target":"2% target · 14 days", "balanced_slow":"5% target · 14 days",
                   "balanced_fast":"5% target · 7 days", "higher_target":"10% target · 14 days"}
    sensitivity=read("behavior_sensitivity.csv")
    summaries={key:stats([x for x in sensitivity if x["profile"]==key]) for key in profile_names}
    comparison=read("signal_comparison.csv")
    default=[x for x in comparison if x["signal"]=="combined"]
    signal_stats={s:stats([x for x in comparison if x["signal"]==s])
        for s in ("utilization","coverage","combined")}
    seed=next(x for x in default if x["scenario"]=="seed_only")
    paid_case=next(x for x in read("bootstrap_and_collection.csv") if x["cap"]=="0.5"
        and x["lp_signal"]=="collected" and x["lp_interest_fraction"]=="0.9" and x["scenario"]=="credit_stress")
    baseline_case=next(x for x in default if x["scenario"]=="credit_stress")
    fees=[]
    for ownership in (F(1,10),F(1,2),F(9,10),F(99,100)):
        total=F(200_000); own=total*ownership; fee=own*F(5,10_000)
        refunded_value=(own-fee)*total/(total-fee)
        fees.append(dict(junior_ownership=float(ownership),request_value=float(own),
            quoted_fee=float(fee),net_cost_if_returned_to_junior=float(own-refunded_value),
            net_cost_if_transferred_to_seed=float(fee)))
    files=["screen.csv","behavior_sensitivity.csv","calibration_sensitivity.csv","time_step.csv",
        "signal_comparison.csv","queue_fees.csv","bootstrap_and_collection.csv"]
    runs=sum(len(read(name)) for name in files)
    current_defaults=json.loads((ROOT.parent/"v1-controller/defaults.json").read_text())
    findings=dict(scenario_parameter_runs=runs,current_defaults=current_defaults,historical_test_candidate=dict(signal="(J / D_total) * (1 - U_asset)",
        target=.05,band=[.045,.055],rate_half_life_days=14,initial_apr=.04,min_apr=.01,max_apr=.50,
        new_borrow_utilization_limit=.80,seed_fraction=.001,notice_days=[7,10],claim_window_days=2,
        queue_fee_bps=5,queue_fee_destination="permanent junior seed",status="historical experiment; superseded by per-market V1 defaults"),
        behavior_summaries=summaries,default_summaries=stats(default),signal_summaries=signal_stats,seed_only_case=seed,
        received_revenue_credit_case=paid_case,forecast_revenue_credit_case=baseline_case,
        fee_ownership_examples=fees,simulation_limits=["Synthetic LP/borrower behavior",
        "Exogenous credit loss and inventory shock, not AMM/auction execution",
        "No empirical prediction or safety guarantee","Exact ledger excludes integer rounding and native compute costs"],
        native_open_items=["Verified insolvency index and resistance to repeated dust restrictions",
        "Per-loan income-history catch-up cost and data availability",
        "Atomic token-generation rollover and source-specific hLP integration",
        "Integer rounding, account sizes, native tests and full CI"])
    (OUT/"findings.json").write_text(json.dumps(findings,indent=2)+"\n")

    bg,ink,muted,line="#FBF9F7","#28231F","#4D4641","#E6E1DA"
    plt.rcParams.update({"font.family":"DejaVu Sans","font.size":10,"text.color":ink,
        "axes.labelcolor":ink,"xtick.color":muted,"ytick.color":muted,
        "axes.edgecolor":line,"axes.spines.top":False,"axes.spines.right":False,
        "axes.facecolor":bg,"figure.facecolor":bg,"grid.color":line,"svg.fonttype":"path"})
    try:
        from fontTools.ttLib import TTFont
        from matplotlib import font_manager
        font_path=Path("/private/tmp/dusk-tranche-calibration-font.ttf")
        font=TTFont(BRAND/"msaansvf.woff2");font.flavor=None;font.save(font_path)
        font_manager.fontManager.addfont(str(font_path))
        plt.rcParams["font.family"]=font_manager.FontProperties(fname=str(font_path)).get_name()
    except (ImportError, OSError, RuntimeError):
        pass

    fig,axes=plt.subplots(1,2,figsize=(11.4,4.2),layout="constrained")
    names=list(profile_names)
    labels=[profile_names[x].replace(" · ","\n") for x in names]
    colors=["#B8B0A7",ink,"#70675F","#D1CBC3"]
    for ax,key,title,formatter in [(axes[0],"borrow","Borrowing supported",FuncFormatter(lambda x,pos:f"${x/1000:.0f}k")),
                                  (axes[1],"apr","Borrower interest",PercentFormatter(1))]:
        values=[summaries[n][key] for n in names]
        ax.bar(range(4),values,color=colors,width=.56,zorder=3)
        ax.set_xticks(range(4),labels)
        ax.set_title(title,loc="left",fontsize=14,pad=16)
        ax.yaxis.set_major_formatter(formatter)
        ax.grid(axis="y",zorder=0)
    fig.savefig(OUT/"tradeoffs.svg",bbox_inches="tight")
    fig.savefig(OUT/"tradeoffs.png",dpi=150,bbox_inches="tight")
    plt.close(fig)

    fig,ax=plt.subplots(figsize=(11.4,4.1),layout="constrained")
    for scenario,label,color,style in [("mature","Mature pool",ink,"-"),
        ("bootstrap_slow","Slow junior arrival","#70675F","--"),
        ("seed_only","Permanent seed only","#AFA59B",":")]:
        rows=read(f"trace_{scenario}.csv")
        ax.plot([float(x["day"]) for x in rows],[float(x["rate_quote"]) for x in rows],
                label=label,color=color,linestyle=style,lw=2.4)
    ax.set_xlabel("Day")
    ax.set_ylabel("Quote borrow APR")
    ax.yaxis.set_major_formatter(PercentFormatter(1))
    ax.grid(axis="y")
    ax.legend(frameon=False,loc="upper left")
    ax.set_title("The seed helps bootstrap junior; it does not replace junior participation",loc="left",pad=18,fontsize=13)
    fig.savefig(OUT/"bootstrap.svg",bbox_inches="tight")
    fig.savefig(OUT/"bootstrap.png",dpi=150,bbox_inches="tight")
    plt.close(fig)

    def svg(name):
        raw=(OUT/name).read_text();return raw[raw.index("<svg"):]
    logo=base64.b64encode((BRAND/"omnipair-lockup.svg").read_bytes()).decode()
    font=base64.b64encode((BRAND/"msaansvf.woff2").read_bytes()).decode()
    table="".join(f"<tr><td>{profile_names[k]}</td><td>{money(v['borrow'])}</td><td>{pct(v['apr'])}</td><td>{money(v['senior_loss'])}</td></tr>" for k,v in summaries.items())
    signal_table="".join(f"<tr><td>{k.capitalize()}</td><td>{money(v['borrow'])}</td><td>{pct(v['apr'])}</td><td>{money(v['senior_loss'])}</td></tr>" for k,v in signal_stats.items())
    fee_table="".join(f"<tr><td>{pct(x['junior_ownership'])}</td><td>{money(x['request_value'])}</td><td>${x['quoted_fee']:.2f}</td><td>${x['net_cost_if_returned_to_junior']:.2f}</td></tr>" for x in fees)
    html=f'''<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Junior capital — research and market configuration</title>
<style>@font-face{{font-family:Saans;src:url(data:font/woff2;base64,{font}) format('woff2')}}
:root{{color-scheme:light}}*{{box-sizing:border-box}}body{{margin:0;background:{bg};color:{ink};font:16px/1.6 Saans,Arial,sans-serif}}main{{max-width:1100px;margin:auto;padding:54px 42px 90px}}header img{{width:160px;height:auto}}.eyebrow{{font-size:11px;letter-spacing:.12em;text-transform:uppercase;color:{muted};margin:38px 0 12px}}h1{{font-size:46px;line-height:1.1;letter-spacing:-.035em;margin:0 0 24px;max-width:750px}}h2{{font-size:27px;line-height:1.25;letter-spacing:-.02em;margin:46px 0 18px}}h3{{font-size:18px;margin:0 0 10px}}p{{max-width:900px}}.lead{{font-size:21px;max-width:880px}}.rule{{border-top:1px solid {line};margin-top:34px}}.grid{{display:grid;grid-template-columns:1fr 1fr;gap:24px;margin:26px 0}}.panel{{border:1px solid {line};border-radius:12px;padding:24px;background:#fff}}.muted,.caption{{color:{muted};font-size:13px}}table{{width:100%;border-collapse:collapse;font-size:14px}}th,td{{text-align:left;padding:13px 12px;border-bottom:1px solid {line}}}th{{color:{muted};font-weight:500}}td:not(:first-child){{font-variant-numeric:tabular-nums}}.figure{{margin:24px 0}}.figure svg{{width:100%;height:auto}}code{{font:14px ui-monospace,monospace;background:#F4F1EC;padding:3px 6px;border-radius:4px}}a{{color:inherit;text-underline-offset:3px}}li{{margin:9px 0}}.tag{{font-size:12px;border:1px solid {line};border-radius:30px;padding:6px 12px;display:inline-block}}@media(max-width:700px){{main{{padding:26px 18px}}h1{{font-size:34px}}.grid{{grid-template-columns:1fr}}table{{font-size:12px}}th,td{{padding:9px 5px}}}}</style></head>
<body><main><header><img src="data:image/svg+xml;base64,{logo}" alt="Omnipair"></header>
<div class="eyebrow">Dusk · design experiment · 31 October 2026</div><h1>Junior capital.<br>Configurable per market.</h1>
<span class="tag">Research and current specification · native integration in progress</span>
<p class="lead">Use the V1 rate-controller mechanism with governance-adjustable parameters for each market. Omitted inputs default to a 14-day half-life, 4% initial APR, a 2% floor and no configured ceiling. The default 5–6.25% signal band expresses 20–25% junior capital at 80% reference utilization.</p>
<p>{runs:,} synthetic simulations were run, including repeated baseline configurations. These earlier experiments used different rate bounds and half-lives. They remain historical evidence and do not validate the new uncapped defaults. The current specification is <a href="../v1-controller/README.md">documented separately</a>; there is no universal immutable configuration to optimize.</p>
<div class="grid"><div class="panel"><h3>Current market defaults</h3><table>
<tr><td>Signal per borrowed asset</td><td><code>(J / D_total) × (1 − U_asset)</code></td></tr>
<tr><td>Signal lower / upper bound</td><td>5% / 6.25%</td></tr><tr><td>Rate doubles or halves outside band</td><td>{current_defaults["model"]["half_life_ms"] // 86_400_000} days; adjustable per market</td></tr>
<tr><td>Starting / minimum / maximum APR</td><td>4% / 2% / uncapped<br><span class="muted">Defaults when omitted; configurable per market</span></td></tr><tr><td>New-borrow utilization limit</td><td>80% per asset</td></tr></table></div>
<div class="panel"><h3>Capital and exits</h3><table><tr><td>Permanent junior seed</td><td>0.1% of initial creator liquidity</td></tr>
<tr><td>Notice / exercise window</td><td>7–10 days / 2 days</td></tr><tr><td>Request/renewal fee</td><td>0.05%, transferred to the seed</td></tr>
<tr><td>No live junior</td><td>Borrowing remains available; new junior fees rebuild funded seed capital</td></tr>
<tr><td>Liquidation allocation</td><td>Former insurance-fill amount → junior; executor and protocol cuts unchanged</td></tr></table></div></div>
<p class="caption">Both band bounds, half-life, initial APR, minimum APR and maximum APR are configurable per market. Omitted creation inputs use these defaults; valid supplied values override them. Governance changes apply prospectively and do not reset an existing market to its initial APR. Rate adjustment starts immediately outside the band. The 5–6.25% band stays fixed until governance changes it; it is not recalculated from live utilization. The 0.05% queue fee is transferred to the permanent junior seed. The source review also records precision and update-ordering fixes needed when porting V1.</p>
<div class="rule"></div><h2>Earlier controller comparisons</h2>
<div class="figure">{svg('tradeoffs.svg')}</div>
<p class="caption">Equal-weight behavioral and shock-timing cases, starting from a $1m pool plus $20k of insurance. The averages are a comparison convention, not probabilities or forecasts.</p>
<table><thead><tr><th>Controller</th><th>Average borrowing</th><th>Mean borrower APR</th><th>Largest senior credit loss</th></tr></thead><tbody>{table}</tbody></table>
<p>The 2% target supports more borrowing but allows a thinner junior buffer. The 7-day response can reduce losses in some stress timings; the 14-day response changes direction less often in the initial ten scenarios. Raising the target to 10% increases costs and reduces borrowing without removing early-bootstrap exposure.</p>
<p><strong>Do not read the initial zero-loss cases as a guarantee.</strong> With the same 5% / 14-day parameters, the credit-stress case passes {money(float(paid_case['senior_loss']))} to senior when junior providers respond to collected fees, versus {money(float(baseline_case['senior_loss']))} when they respond to projected revenue. The behavioral assumption matters.</p>
<h2>Does the coverage signal earn its place?</h2>
<table><thead><tr><th>Pricing signal</th><th>Average borrowing</th><th>Mean borrower APR</th><th>Largest senior credit loss</th></tr></thead><tbody>{signal_table}</tbody></table>
<p>Across the initial ten scenarios, adding coverage reduces lending and raises borrowing costs compared with the utilization-only controller, while reducing senior losses in the tested crash. That is a trade-off, not free capital efficiency. Whether users prefer it depends on the price of protection and the availability of junior capital.</p>
<p class="caption">All three use the same capital budget, revenue split, seed and adaptive-rate shape. Targets are 80% utilization, 25% coverage, or the combined 5% signal; these align only at 80% utilization. This isolates candidate signal behavior and is not a benchmark against Dusk’s current Morpho-like rate implementation. Only the four combined-controller profiles above received the broader behavioral sensitivity sweep.</p>
<h2>The seed is productive, but small</h2><div class="figure">{svg('bootstrap.svg')}</div>
<p>With no user joining junior, the seed grows from $1,000 to {money(float(seed['final_seed']))} over 90 simulated days. Average borrowing falls to {money(float(seed['avg_borrow']))}; peak borrow APR reaches {pct(float(seed['peak_apr']))}. The seed resolves who owns early junior fees. It does not supply the underwriting depth of a mature junior tranche.</p>
<div class="panel"><h3>Show the deposit’s rate, not just the tiny seed’s rate</h3>
<p>Suppose annual net LP revenue is $40,000. Junior receives $20,000. A $1,000 seed alone shows a 2,000% annualized rate. After a user converts $100,000 into junior, the same revenue supports about 19.8% per dollar of junior capital. Neither number is a guaranteed APY.</p>
<p class="muted">Display a prospective rate for the user’s selected amount. Label accrued estimates separately from collected income. Zero borrowing and zero swap revenue means zero yield; do not create a positive APY from accounting alone. Seed compounding and future deposits change the rate over time.</p></div>
<h2>A fee must have a real recipient</h2><p>At 5 bp, a $10,000 request costs $5 if those shares move to the permanent seed. Returning the fee to remaining junior holders partially refunds the requester:</p>
<table><thead><tr><th>Requester owns</th><th>Request value</th><th>Quoted fee / cost to seed</th><th>Net cost if returned to junior</th></tr></thead><tbody>{fee_table}</tbody></table>
<p>Repeatedly renewing an entire position every 9–12 days would cost roughly 1.5–2.0% of its value per year at 5 bp, before compounding or price changes. That is meaningful friction, not proof that strategic exits are unprofitable. The macro model does not optimize adversarial queue timing.</p>
<h2>What the accounting prototype establishes</h2><p>Eleven additional V1-controller reference tests pass, covering configurable bounds, optional creation inputs, uncapped rates, longer half-lives and prospective governance changes. The earlier thirty-one tests cover exact two-token balances, one shared junior budget, burning srLP on junior entry, historical income after transfers, partial collections, unpaid-interest writeoffs, non-redeemable seed claims, rebuilding junior after exhaustion, and preserving unused claim-window time during restrictions.</p>
<p>Income preparation and claiming are capped at eight records per call. Historical share checkpoints preserve the original owner’s entitlement. This bounds work per call; it does not eliminate history storage, catch-up transactions, or the need to verify data access and compute cost in Solana.</p>
<div class="panel"><h3>Still required before native integration</h3><ul>
<li>A reliable insolvency index and a rule that cannot be held indefinitely by repeated tiny distressed positions. The prototype tests the clock, not that detection/griefing solution.</li>
<li>Native handling of long-idle loan income history, integer rounding, token-generation rollover after junior exhaustion, and hLP integration. Empty-senior routing remains a separate unresolved rule.</li>
<li>Native state-transition, SDK/interface and full CI checks. No Rust program or deployed configuration has been changed for this experiment.</li></ul></div>
<h2>Inspect the evidence</h2><p><a href="../v1-controller/defaults.json">Current market defaults</a> · <a href="results/findings.json">Historical findings</a> · <a href="results/screen.csv">810 screening runs</a> · <a href="results/behavior_sensitivity.csv">Behavior and timing sensitivity</a> · <a href="results/time_step.csv">Time-step comparison</a> · <a href="accounting.py">Exact accounting prototype</a> · <a href="PREANALYSIS.md">Assumptions fixed before screening</a></p>
<p class="caption">Synthetic study: constant assumed net swap-income rates, specified borrower/LP responses, exogenous credit losses, and a separate solvent-liquidation fee event. Insurance has asset-specific balances and the existing 20% event draw limit in the base cases. The simulation does not execute Dusk’s AMM, liquidation auction, transaction ordering or MEV. No empirical default probability is inferred.</p>
</main></body></html>'''
    (ROOT/"review.html").write_text(html)
    print(f"Rendered review.html for {runs} simulation runs")


if __name__=="__main__":main()
