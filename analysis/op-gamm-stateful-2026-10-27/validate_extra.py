from run_cycles import *
import random

def repayment_tests():
 r=RPC();rng=random.Random(20261027);rows=[]
 for index in [10**9,10**9+1,1000073048,1100000000,1500000000,3000000000,20000000000]:
  for n in [1,2,10,101,1000003,10**12]:
   for phase in [0,1,17,1234567]:
    position=n;aggregate=n+phase;initial=aggregate*index//10**9;paid=0;steps=0;reject=0
    for k in range(128):
     if position==0:break
     maximum=max(1,min(10**15,rng.choice([1,2,3,17,max(1,position//7),position])*index//10**9))
     q=r(op='repay',position=str(position),aggregate=str(aggregate),index=str(index),maximum=str(maximum))
     if 'error' in q:reject+=1;continue
     burn=int(q['burn']);assert 0<burn<=position and 0<q['cash']<=maximum
     assert q['cash']==aggregate*index//10**9-(aggregate-burn)*index//10**9
     paid+=q['cash'];position-=burn;aggregate-=burn;steps+=1
     assert paid==initial-aggregate*index//10**9
    rows.append(dict(index=index,initial_position=n,other_shares=phase,steps=steps,rejected=reject,paid=paid,aggregate_debt_reduced=initial-aggregate*index//10**9,remaining=position))
 dump('repayment_rounding',rows);r.close();return rows

def extra_carry():
 rows=[];r=RPC();db,dq=9,6
 for mode in ['native','blend','bounded']:
  a=Research(r,4,mode);s=r(op='new',kind=6,db=db,dq=dq,amp=4);init=s
  for cycle in range(1,33):
   best=None;reject=0
   for d in [0,1]:
    for raw in ([1,10,100,1000,10000,100000,500000] if d==0 else [1,2,10,60,600,6000,30000]):
     z,err=roundtrip(a,s,d,raw,db,dq,True)
     if err:reject+=1;continue
     if best is None or z[0]>best[0]:best=z
   if best is None:
    rows.append(dict(mode=mode,cycle=cycle,best_profit='',cash_loss='',funding_due_quote='',rejections=reject,status='no fill'));break
   bestprofit,atoms,w,v,raw,d=best
   rows.append(dict(mode=mode,cycle=cycle,best_profit=bestprofit,cash_loss=wealth(init,db,dq)-wealth(w,db,dq),funding_due_quote=(w['meta'][5]-w['meta'][7])/1e6,rejections=reject,status='accepted'))
   if bestprofit<=0:break
   s=w
 dump('cash_constrained_carry',rows);r.close();return rows

def nav(s,P=60):
 m,b=s['meta'],s['books'];B=b[0]/1e9;Q=b[1]/1e6
 return [B*m[1]/m[0]+(Q*m[1]/m[0]-m[5]/1e6)/P,Q*m[2]/m[0]+(B*m[2]/m[0]-m[6]/1e9)*P]
def nav_guard():
 r=RPC();rows=[]
 for k in [0,1,2,3,4,5]:
  for mode in ['blend','bounded']:
   a=Research(r,4,mode);s=r(op='new',kind=k,db=9,dq=6,amp=4)
   for O in [60,600]:
    P=a.mark(s['point'],a.curve(O))/1e9;before=nav(s,P)
    for d in [0,1]:
     for raw in sizes(s,d,9,6)[::2]:
      z=a.quote(s,d,raw,O,False,carry=k==2)
      if 'error' in z:
       rows.append(dict(kind=KINDS[k],mode=mode,O=O,d=d,raw=raw,curve_status=brief(z),base_nav_delta='',quote_nav_delta='',nav_guard='not reached'));continue
      after=nav(z,P);delta=[after[j]-before[j] for j in [0,1]]
      # Three raw atoms per asset, valued at the same pre-trade mark.
      tol=[3e-9+3e-6/P,3e-6+3e-9*P]
      ok=all(s['meta'][9]&(1<<j) or delta[j]>=-tol[j] for j in [0,1])
      rows.append(dict(kind=KINDS[k],mode=mode,O=O,d=d,raw=raw,curve_status='accepted',base_nav_delta=delta[0],quote_nav_delta=delta[1],nav_guard='pass' if ok else 'reject'))
 dump('active_nav_diagnostic',rows);r.close();return rows
if __name__=='__main__':
 for f in [repayment_tests,extra_carry,nav_guard]:
  r=f();print(f.__name__,len(r),flush=True)
