"""Verify current reference settings and record the scoped test evidence."""
from pathlib import Path
from dataclasses import asdict
import hashlib
import json
import subprocess
import sys
from controller import RateModel

ROOT=Path(__file__).resolve().parent


def main():
    settings=json.loads((ROOT/"defaults.json").read_text())
    assert settings["model"]==asdict(RateModel())
    assert settings["junior_pool_share_reference_band"]==[.2,.25]
    assert settings["reference_utilization"]==.8
    result=subprocess.run([sys.executable,"-m","unittest","test_controller.py","-v"],
        cwd=ROOT,capture_output=True,text=True)
    (ROOT/"tests.log").write_text(result.stdout+result.stderr)
    assert result.returncode==0,result.stdout+result.stderr
    assert "Ran 11 tests" in result.stderr
    repro=json.loads((ROOT/"v1-reproduction.json").read_text())
    assert "14,true,0,panic" in repro["output"]
    assert "30,true,0,panic" in repro["output"]
    sources=[x for x in ROOT.iterdir() if x.is_file() and x.name!="verification.json"]
    hashes={x.name:hashlib.sha256(x.read_bytes()).hexdigest() for x in sorted(sources)}
    (ROOT/"verification.json").write_text(json.dumps({"status":"passed","tests":11,
        "defaults_match_reference":True,"v1_zero_coefficient_reproduction_present":True,
        "hashes":hashes,"scope":"Mathematical reference only; native Dusk unchanged"},indent=2)+"\n")
    print("11 controller contract tests passed; defaults and V1 reproduction checked.")


if __name__=="__main__":main()
