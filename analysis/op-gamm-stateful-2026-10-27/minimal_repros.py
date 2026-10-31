"""Two isolated host-native/shadow repros; all candidate switches default off."""
from run_cycles import *
def run_one(issue,fixed):
 native=issue=='native_frozen';r=RPC(fix_frozen=fixed and native,fix_carry=fixed and not native)
 k=3 if native else 2;mode='native' if native else 'bounded';a=Research(r,4,mode)
 s=r(op='new',kind=k,db=9,dq=6,amp=4);raw=(4200 if native else 10)*10**9
 v=a.quote(s,0,raw,60,True,carry=not native);assert 'out' in v,v
 w=a.quote(v,1,v['out'],60,True,carry=not native);assert 'out' in w,w
 profit=(w['out']-raw)/1e9*60
 assert frozen_signature(s)==frozen_signature(w)
 assert abs(wealth(w,9,6)-wealth(s,9,6)+profit)<1e-8
 if fixed:assert profit<=0,(issue,profit)
 else:assert profit>0,(issue,profit)
 result=dict(issue=issue,fixed=fixed,input_base_atoms=raw,first_output_quote_atoms=v['out'],final_base_atoms=w['out'],profit_base_atoms=w['out']-raw,profit_quote_value=profit,lp_loss_quote=wealth(s,9,6)-wealth(w,9,6),initial=s,after_first=v,after_roundtrip=w)
 r.close();return result
if __name__=='__main__':
 out=[run_one(k,f) for k in ['native_frozen','oracle_carry'] for f in [False,True]]
 (OUT/'minimal_repros.json').write_text(json.dumps(out,indent=2))
 for v in out:print(v['issue'],v['fixed'],v['first_output_quote_atoms'],v['final_base_atoms'],v['profit_quote_value'])
