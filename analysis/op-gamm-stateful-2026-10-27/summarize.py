from pathlib import Path
import json,csv,collections,hashlib,subprocess,difflib,platform
import numpy as np
import os
os.environ.setdefault('MPLCONFIGDIR','/private/tmp/dusk-op-gamm-mpl')
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
R=Path(__file__).resolve().parent;O=R/'results';PIN='77877d576018fd2bd8191d13f05534b0484e2282'
def read(n):return list(csv.DictReader((O/(n+'.csv')).open()))
minimal=json.loads((O/'minimal_repros.json').read_text());grid=read('candidate_fix_search');cy=read('candidate_fix_cycles');diff=read('candidate_integer_differential');regr=read('regressions');repay=read('repayment_rounding');fault=read('fault_recovery_cycles_fixed')
summary={'pin':PIN,'python':platform.python_version(),'numpy':np.__version__,'matplotlib':matplotlib.__version__,
'minimal':[{k:v[k] for k in ['issue','fixed','input_base_atoms','first_output_quote_atoms','final_base_atoms','profit_quote_value']} for v in minimal],
'grid':{'configurations':len(grid),'completed_roundtrip_trials':sum(int(x['valid']) for x in grid),'rejected_trials':sum(int(x['rejected']) for x in grid),'positive_cases':[x for x in grid if x['best_profit'] and float(x['best_profit'])>0]},
'fixed_repeated_cycles':len([x for x in cy if x['fixed']=='True' and x['status']=='accepted']),
'fixed_repeated_positive_cycles':len([x for x in cy if x['fixed']=='True' and x['profit'] and float(x['profit'])>0]),
'integer_differential':dict(collections.Counter(x['result'] for x in diff)),
'regressions':len(regr),'repayment_cases':len(repay),'repayment_steps':sum(int(x['steps']) for x in repay),
'fixed_fault_cycles':len(fault),'fault_ledger_max_abs_quote_residual':max(abs(float(x['ledger_residual'])) for x in fault),
'claims':'Controlled host-native ledger and shadow pricing tests, not SVM certification or current-main findings.'}
(O/'summary.json').write_text(json.dumps(summary,indent=2))
plt.rcParams.update({'font.family':'DejaVu Sans','font.size':10})
fig,axes=plt.subplots(1,2,figsize=(11,4.2),layout='constrained')
for ax,kind,mode,title in zip(axes,['frozen_base','carried'],['native','bounded'],['Native frozen hLP: same 4,200-base round trip','Oracle carry: same 10-base round trip']):
 for fixed,color,label in [('False','#d95b43','Original accounting'),('True','#19846b','Research fix')]:
  a=[x for x in cy if x['kind']==kind and x['mode']==mode and x['fixed']==fixed and x['recovery']=='False' and x['status']=='accepted']
  ax.plot([int(x['cycle']) for x in a],[float(x['cumulative']) for x in a],color=color,lw=2.4,marker='o',ms=3,label=label)
 ax.set_title(title,fontsize=11,loc='left',pad=14);ax.set_xlabel('Repeated round trips');ax.set_ylabel('Cumulative attacker profit (quote units)');ax.spines[['top','right']].set_visible(False);ax.grid(alpha=.18);ax.set_xlim(0,33)
 axes[0].legend(frameon=False,loc='upper left')
fig.suptitle('Accounting fixes remove the repeated extraction in these fixtures',fontsize=14,x=.04,ha='left')
axes[0].annotate('Next request fails for liquidity',xy=(9,171276.99759486),xytext=(12,145000),fontsize=9,arrowprops=dict(arrowstyle='->',color='#555'),color='#555')
fig.savefig(O/'isolated-fixes.png',dpi=170);fig.savefig(O/'isolated-fixes.svg');plt.close(fig)
# Reviewable patch is for archived source only; switches are test instrumentation.
patch=''
for rel in ['transitions/liquidity/hlp/integrated.rs','transitions/liquidity/hlp/engine.rs']:
 old=subprocess.check_output(['git','show',PIN+':programs/dusk/src/'+rel],text=True)
 new=(R/'engine-snapshot/programs/dusk/src'/rel).read_text()
 patch+=''.join(difflib.unified_diff(old.splitlines(True),new.splitlines(True),fromfile='pinned/'+rel,tofile='research-snapshot/'+rel))
(R/'candidate-accounting.patch').write_text(patch)
manifest={str(p.relative_to(R)):hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(R.rglob('*')) if p.is_file() and ('engine-snapshot' not in p.parts) and ('__pycache__' not in p.parts) and p.suffix in ['.py','.rs','.toml','.csv','.json','.patch','.md','.svg','.png'] and p.name!='manifest.json'}
(O/'manifest.json').write_text(json.dumps(manifest,indent=2))
print(json.dumps(summary,indent=2))
