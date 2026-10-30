import sys,json,math,collections,time
from experiment import *
KINDS=['clean','accrued','carried','frozen_base','frozen_quote','frozen_both']
def brief(e):
 s=e.get('error',e.get('state_error',''))
 return s.split('error_name: "')[1].split('"')[0] if 'error_name: "' in s else s

def wealth(s,db,dq,S=60):
 # All ordinary+hLP depositors consolidated: funding debt/claims cancel internally.
 # Public receivables are separately identifiable; no cash created by marking them.
 m,b=s['meta'],s['books']
 return (m[3]+b[2]+b[10])/10**db*S+(m[4]+b[3]+b[11])/10**dq

def navs(s,db,dq,S=60):
 m,b=s['meta'],s['books'];supply=m[0]
 base=(b[0]*m[1]//supply)/10**db*S+(b[1]*m[1]//supply-m[5])/10**dq
 quote=(b[1]*m[2]//supply)/10**dq+(b[0]*m[2]//supply-m[6])/10**db*S
 ordinary=((b[0]*(supply-m[1]-m[2])//supply)/10**db*S+(b[1]*(supply-m[1]-m[2])//supply)/10**dq)
 return [base,quote,ordinary]
def frozen_signature(s):
 m,b=s['meta'],s['books'];v=[]
 for j in [0,1]:
  if m[9]&(1<<j):v.extend([m[1+j],b[6+j],m[7+j],b[8+j]])
 return v+[m[9],b[12],b[13]]
def sizes(s,d,db,dq):
 # Economically equal input notional per direction, plus exact atom probes.
 dec=db if d==0 else dq
 amount=s['s'][d]/1e9 if 's' in s else s['point'][d]/1e9
 return sorted({1,2,3,10,max(1,int(1e-7*amount*10**dec)),*[max(1,int(f*amount*10**dec)) for f in [1e-5,.0001,.001,.01,.05,.15,.35]]})
def roundtrip(a,s,d,raw,db,dq,carry,policy='materialized'):
 v=a.quote(s,d,raw,60,True,carry=carry,policy=policy)
 if 'error' in v:return None,brief(v)
 w=a.quote(v,1-d,v['out'],60,True,carry=carry,policy=policy)
 if 'error' in w:return None,brief(w)
 atoms=w['out']-raw;usd=atoms/10**(db if d==0 else dq)*(60 if d==0 else 1)
 return (usd,atoms,w,v,raw,d),None

def fixed_cycles():
 rows=[];search=[];start=time.time()
 for db,dq in [(9,6),(6,9),(2,2)]:
  for k in range(6):
   for mode in ['native','blend','bounded']:
    r=RPC();a=Research(r,4,mode);s=r(op='new',kind=k,db=db,dq=dq,amp=4)
    if 'error' in s:raise ValueError(s)
    init=s;cum=0;valid=0;reject=collections.Counter();positive=0
    for cycle in range(1,17):
     best=None;found=0
     for d in [0,1]:
      for raw in sizes(s,d,db,dq):
       z,err=roundtrip(a,s,d,raw,db,dq,k==2)
       if err:reject[err]+=1;continue
       valid+=1;found+=1
       if best is None or z[0]>best[0]:best=z
     if best is None:break
     # Attacker can abstain. Retain the best trial for rounding/liveness evidence.
     if cycle==1:search.append(dict(db=db,dq=dq,kind=KINDS[k],mode=mode,best_usd=best[0],best_atoms=best[1],direction=best[5],input_atoms=best[4],valid_trials=found))
     if best[0]<=0:break
     positive+=1;cum+=best[0];s=best[2]
     assert frozen_signature(s)==frozen_signature(init)
     rows.append(dict(db=db,dq=dq,kind=KINDS[k],mode=mode,cycle=cycle,direction=best[5],input_atoms=best[4],first_output_atoms=best[3]["out"],final_output_atoms=best[2]["out"],profit=best[0],cumulative=cum,wealth_change=wealth(s,db,dq)-wealth(init,db,dq),base_hlp_nav=navs(s,db,dq)[0],quote_hlp_nav=navs(s,db,dq)[1],ordinary_claim=navs(s,db,dq)[2]))
    search.append(dict(db=db,dq=dq,kind=KINDS[k],mode=mode,best_usd=cum,best_atoms='',direction='summary',input_atoms='',valid_trials=valid))
    print('fixed',db,dq,k,mode,'profit',round(cum,8),'positive',positive,'reject',dict(reject),flush=True)
    (OUT/'fixed_progress.json').write_text(json.dumps(dict(seconds=time.time()-start,case=[db,dq,k,mode],rejections=dict(reject))))
    r.close()
 dump('fixed_reference_cycles',rows);dump('roundtrip_search',search)
 return rows,search

def best_external(a,s,O,db,dq,carry,policy='materialized',fraction=1):
 best=None;rejected=0
 for d in [0,1]:
  for raw in sizes(s,d,db,dq):
   raw=max(1,int(raw*fraction))
   z=a.quote(s,d,raw,O,False,carry=carry,policy=policy)
   if 'error' in z:rejected+=1;continue
   profit=z['out']/10**(dq if d==0 else db)*(1 if d==0 else 60)-raw/10**(db if d==0 else dq)*(60 if d==0 else 1)
   if best is None or profit>best[0]:best=(profit,d,raw,z)
 if best is None or best[0]<=0:return s,0,rejected,None
 profit,d,raw,z=best
 z=a.quote(s,d,raw,O,True,carry=carry,policy=policy)
 assert 'error' not in z
 return z,profit,rejected,d

def fault_cycles(fixed=False):
 rows=[];recoveries=[]
 for k in [1,2,3,4,5]:
  for mode in ['native','blend','bounded']:
   for weak in [False,True]:
    r=RPC(fix_frozen=fixed,fix_carry=fixed and mode!='native');a=Research(r,4,mode);db,dq=9,6;s=r(op='new',kind=k,db=db,dq=dq,amp=4);init=s;cum=0;accrued=0;recoverflow=0
    for cycle in range(1,9):
     before=wealth(s,db,dq)
     # Native accrual, measured independently from extraction. No debt forgiven.
     adv=r(op='advance',id=s['id'],slot=500000+cycle*9000)
     advance_error=brief(adv)
     if 'error' not in adv:s=adv
     accrued+=wealth(s,db,dq)-before
     # Attacker chooses report polarity and best input direction/size in finite grid.
     options=[]
     for O in [6,600]:
      z,p,e,d=best_external(a,s,O,db,dq,k==2)
      options.append((p,O,z,e,d))
     p,O,z,reject,d=max(options,key=lambda v:v[0]);s=z;cum+=p
     faultprofit=p
     # Correct report resumes. Weak recovery is 10% of the same input-size grid.
     s,honest,reject2,d2=best_external(a,s,60,db,dq,k==2,fraction=.1 if weak else 1)
     cum+=honest
     # A price/report recovery alone never clears a frozen vault. Attempt native
     # funded hLP recovery only with competitive buyers; record failures honestly.
     status=[]
     if not weak:
      for side in [0,1]:
       rec=r(op='recover',id=s['id'],side=side,commit=False)
       if 'error' in rec:status.append(brief(rec));continue
       payment,collateral,discount=rec['recovery']
       gain=collateral/10**(db if side==0 else dq)*(60 if side==0 else 1)-payment/10**(dq if side==0 else db)*(1 if side==0 else 60)
       status.append('unprofitable' if gain<0 else 'available')
       if gain>=0:
        rec=r(op='recover',id=s['id'],side=side);recoverflow+=gain;s=rec
       recoveries.append(dict(kind=KINDS[k],mode=mode,weak=weak,cycle=cycle,side=side,payment=payment,collateral=collateral,bidder_gain=gain,executed=gain>=0))
     rows.append(dict(kind=KINDS[k],mode=mode,weak=weak,cycle=cycle,report=O,fault_profit=faultprofit,recovery_arb=honest,cumulative_extraction=cum,public_interest_accrual=accrued,recovery_bidder_gain=recoverflow,lp_wealth_change=wealth(s,db,dq)-wealth(init,db,dq),ledger_residual=wealth(s,db,dq)-wealth(init,db,dq)-accrued+cum+recoverflow,base_hlp_nav=navs(s,db,dq)[0],quote_hlp_nav=navs(s,db,dq)[1],frozen_mask=s['meta'][9],rejections=reject+reject2,accrual_error=advance_error,recovery_status=';'.join(status)))
    print('fault',k,mode,weak,round(cum,4),round(rows[-1]['ledger_residual'],9),flush=True);r.close()
 dump('fault_recovery_cycles_fixed' if fixed else 'fault_recovery_cycles',rows);dump('funded_recovery_attempts_fixed' if fixed else 'funded_recovery_attempts',recoveries);return rows

def materialization_sweep():
 rows=[]
 for db,dq in [(9,6),(6,9),(2,2),(0,0)]:
  for k in range(6):
   for mode in ['native','blend','bounded']:
    r=RPC();a=Research(r,4,mode);s=r(op='new',kind=k,db=db,dq=dq,amp=4)
    if 'error' in s:r.close();continue
    for O in [60,600]:
     for d in [0,1]:
      for raw in sizes(s,d,db,dq)[::2]:
       z=a.quote(s,d,raw,O,False,carry=k==2)
       rows.append(dict(db=db,dq=dq,kind=KINDS[k],mode=mode,O=O,direction=d,input_atoms=raw,status=brief(z) or 'accepted',output_atoms=z.get('out',''),du=z.get('du','')))
    r.close()
 dump('materialization_sweep',rows);return rows
if __name__=='__main__':
 which=sys.argv[1] if len(sys.argv)>1 else 'all'
 if which in ['sweep','all']:materialization_sweep()
 if which in ['fixed','all']:fixed_cycles()
 if which in ['fault','all']:fault_cycles()
