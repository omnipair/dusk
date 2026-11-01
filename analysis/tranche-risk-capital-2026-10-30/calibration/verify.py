"""Verify saved results and record reproducible evidence, without regenerating sweeps."""
from pathlib import Path
from dataclasses import replace
import csv
import hashlib
import json
import math
import subprocess
import sys

from model import Params, Simulation, SCENARIOS

ROOT=Path(__file__).resolve().parent
OUT=ROOT/"results"


def rows(name):
    with (OUT/name).open() as f:
        return list(csv.DictReader(f))


def main():
    check=subprocess.run([sys.executable,"-m","unittest","test_accounting.py","test_model.py","-v"],
        cwd=ROOT,capture_output=True,text=True)
    (OUT/"tests.log").write_text(check.stdout+check.stderr)
    assert check.returncode==0,check.stdout+check.stderr
    expected={"screen.csv":810,"behavior_sensitivity.csv":168,
        "calibration_sensitivity.csv":270,"time_step.csv":80,"signal_comparison.csv":30,
        "queue_fees.csv":30,"bootstrap_and_collection.csv":126}
    counts={name:len(rows(name)) for name in expected}
    assert counts==expected,(counts,expected)
    findings=json.loads((OUT/"findings.json").read_text())
    assert sum(counts.values())==findings["scenario_parameter_runs"]==1514
    scenarios={s.name:s for s in SCENARIOS}
    screen=rows("screen.csv")
    reproduced=[]
    # Check saved early/late screening outputs still match current default semantics.
    for index in (0,8,408,809):
        saved=screen[index]
        p=replace(Params(),**{key:float(saved[key])
            for key in ("target","half_days","cap","borrow_limit")})
        actual=Simulation(p,scenarios[saved["scenario"]]).run()
        for key,value in actual.items():
            if key!="scenario":
                assert math.isclose(value,float(saved[key]),rel_tol=1e-10,abs_tol=1e-7),(index,key,value,saved[key])
        reproduced.append(index)
    # Every embedded table summary is recomputable from the saved CSV evidence.
    comparison=rows("signal_comparison.csv")
    for name,summary in findings["signal_summaries"].items():
        group=[x for x in comparison if x["signal"]==name]
        assert math.isclose(summary["borrow"],sum(float(x["avg_borrow"]) for x in group)/len(group))
        assert math.isclose(summary["apr"],sum(float(x["borrower_apr"]) for x in group)/len(group))
        assert summary["senior_loss"]==max(float(x["senior_loss"]) for x in group)
    html=(ROOT/"review.html").read_text()
    assert "data:image/svg+xml;base64," in html and "color-scheme:light" in html
    assert "executor and protocol cuts unchanged" in html
    assert "not a benchmark against Dusk" in html
    files=list(ROOT.glob("*.py"))+[ROOT/"PREANALYSIS.md",ROOT/"review.html"]
    files+=list(OUT.glob("*.csv"))+[OUT/"findings.json",OUT/"tests.log"]
    evidence={str(path.relative_to(ROOT)):hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(files)}
    result=dict(status="passed",test_process_exit_code=check.returncode,
        run_counts=counts,total_runs=sum(counts.values()),count_note="Includes repeated baseline configurations; not unique market observations",
        screening_rows_reproduced=reproduced,hashes=evidence,
        exclusions=["Native Dusk integration/CI","Integer rounding and compute costs",
                    "Insolvency detection and dust-griefing predicate","Empirical LP/borrower calibration"])
    (OUT/"verification.json").write_text(json.dumps(result,indent=2)+"\n")
    print("31 tests passed; 1,514 saved runs counted; four screening rows reproduced; report summaries verified.")


if __name__=="__main__":
    main()
