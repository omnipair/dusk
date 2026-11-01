"""Compile the inspected V1 math in isolation; delete generated binary on exit.

Usage: python3 reproduce_v1.py /path/to/omnipair-rs
No change is made to V1. Anchor account macros are removed from the isolated type;
the RateModel implementation, constants and math helpers are copied unchanged.
"""
from pathlib import Path
import hashlib
import json
import re
import subprocess
import sys
import tempfile

ROOT=Path(__file__).resolve().parent


def function(source,name):
    start=source.index("pub fn "+name+"(")
    body=source.index("{",start)
    depth=1
    i=body+1
    while depth:
        depth+=(source[i]=="{")-(source[i]=="}")
        i+=1
    return source[start:i]


def main():
    repo=Path(sys.argv[1]).resolve()
    paths={name:repo/"programs/omnipair/src"/rel for name,rel in
        [("rate_model","state/rate_model.rs"),("constants","constants.rs"),("math","utils/math.rs")]}
    text={name:path.read_text() for name,path in paths.items()}
    constants="\n".join(re.findall(r"^pub const .*?;",text["constants"],flags=re.M))
    body=text["rate_model"].split("#[cfg(test)]")[0]
    body=body[body.index("pub struct RateModel"):]
    math="\n".join(function(text["math"],name) for name in ("taylor_exp","ceil_div"))
    main_code=r'''
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    for days in [1u64,3,7,14,30] {
        let half=days*MS_PER_DAY;
        let valid=RateModel::validate_rate_params(half,200,0,400);
        let model=RateModel::new(3000,5000,half,200,0,400);
        let value=std::panic::catch_unwind(|| model.calculate_rate(40_000_000,half,900_000_000));
        match value {
            Ok((rate,_))=>println!("{},{},{},{}",days,valid,model.exp_rate,rate),
            Err(_)=>println!("{},{},{},panic",days,valid,model.exp_rate),
        }
    }
}
'''
    source=constants+"\n"+math+"\n"+body+"\n"+main_code
    with tempfile.TemporaryDirectory(prefix="dusk-tranche-v1-repro-") as tmp:
        src=Path(tmp)/"repro.rs";exe=Path(tmp)/"repro"
        src.write_text(source)
        subprocess.run(["rustc","--edition=2021","-Awarnings",str(src),"-o",str(exe)],check=True,capture_output=True,text=True)
        output=subprocess.run([str(exe)],check=True,capture_output=True,text=True).stdout
    result={"local_revision":subprocess.check_output(["git","-C",str(repo),"rev-parse","HEAD"],text=True).strip(),
        "files":{str(paths[k].relative_to(repo)):hashlib.sha256(v.encode()).hexdigest() for k,v in text.items()},
        "harness_sha256":hashlib.sha256(source.encode()).hexdigest(),
        "result_columns":["half_life_days","validation_accepts","integer_exp_rate","APR_NAD_after_one_half_life"],
        "output":output,"expected_mathematical_APR_NAD":80_000_000,
        "scope":"Inspected source behavior only; no deployed-account or bytecode verification"}
    (ROOT/"v1-reproduction.json").write_text(json.dumps(result,indent=2)+"\n")
    print(output,end="")


if __name__=="__main__":main()
