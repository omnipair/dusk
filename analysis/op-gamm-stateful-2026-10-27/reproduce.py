"""Rebuild a pinned research snapshot and run isolated repros. Never edits programs/ or git state."""
from pathlib import Path
import subprocess,tarfile,io,os,sys,argparse,hashlib,json
R=Path(__file__).resolve().parent;REPO=R.parents[1];PIN='77877d576018fd2bd8191d13f05534b0484e2282'
os.chdir(REPO)
p=argparse.ArgumentParser();p.add_argument('--all',action='store_true');p.add_argument('--no-build',action='store_true');args=p.parse_args()
if not args.no_build:
 S=R/'engine-snapshot';S.mkdir(exist_ok=True)
 archive=subprocess.check_output(['git','archive',PIN,'Cargo.toml','Cargo.lock','programs','tests/fixtures'])
 with tarfile.open(fileobj=io.BytesIO(archive)) as t:
  for member in t.getmembers():
   dest=(S/member.name).resolve()
   if not dest.is_relative_to(S.resolve()):raise ValueError('archive path escape')
  t.extractall(S,filter='data')
 SRC=S/'programs/dusk/src'
 # Expose native materialization to the research harness, without replacing its arithmetic.
 f=SRC/'transitions/liquidity/hlp/mod.rs'
 f.write_text(f.read_text()+'\npub(crate) fn research_prepare_end(m:&Market,s:IntegratedCurveState)->Result<ConcentratedHlpTransition>{engine::prepare_concentrated_hlp_transition_from_end(m,s,false,false,0,0)}\n')
 # Separate shadow copy for oracle-valued full NAV. Native extraction remains intact.
 f=SRC/'transitions/amm/mod.rs';base=f.read_text();a=base.index('    fn integrated_curve_state_with_mark_nad(');b=base.index('\n    pub(crate) fn hlp_curve_basis_from_reserves_nad',a)
 fn=base[a:b].replace('fn integrated_curve_state_with_mark_nad(&self) -> Result<(IntegratedCurveState, IntegratedCurveMark)>','pub(crate) fn research_state_with_mark(&self, override_mark:u128) -> Result<IntegratedCurveState>').replace('let price = mark.map(|(_, price)| price).unwrap_or(NAD as u128);','let price = override_mark;').replace('Ok((state, mark))','Ok(state)')
 f.write_text(base+'\n// Research copy: only full-equity mark is substituted.\nimpl Market {\n'+fn+'\n}\n')
 subprocess.run([sys.executable,str(R/'apply_research_fixes.py')],check=True)
 env=dict(os.environ,GIT_REV=PIN,CARGO_TARGET_DIR=str(REPO/'target'))
 with (R/'build.log').open('w') as log:
  subprocess.run(['cargo','build','--offline','--manifest-path',str(R/'native/Cargo.toml')],env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
subprocess.run([sys.executable,str(R/'minimal_repros.py')],check=True)
if args.all:
 for script in ['experiment.py','run_cycles.py','validate_extra.py','test_candidate_fixes.py','regression_checks.py']:
  subprocess.run([sys.executable,str(R/script)],check=True)
 subprocess.run([sys.executable,'-c',"import sys;sys.path.insert(0,"+repr(str(R))+ ");from run_cycles import fault_cycles;fault_cycles(True)"],check=True)
if args.all:subprocess.run([sys.executable,str(R/'summarize.py')],check=True)
print('Research artifacts:',R/'results')
