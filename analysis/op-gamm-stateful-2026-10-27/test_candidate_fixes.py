from minimal_repros import *

def optimized_fixed():
 rows=[];diagnostics=[]
 for amp in [1,4]:
  for db,dq in [(9,6),(6,9),(2,2),(0,0)]:
   for k,mode in [(0,'native'),(1,'native'),(3,'native'),(4,'native'),(5,'native'),(0,'bounded'),(1,'bounded'),(2,'bounded'),(2,'blend'),(3,'bounded'),(4,'bounded'),(5,'bounded')]:
    r=RPC(fix_frozen=True,fix_carry=mode!='native');a=Research(r,amp,mode);s=r(op='new',kind=k,db=db,dq=dq,amp=amp);init=s
    best=None;err=collections.Counter();valid=0
    for d in [0,1]:
     for raw in sizes(s,d,db,dq):
      z,e=roundtrip(a,s,d,raw,db,dq,k==2)
      if e:err[e]+=1;continue
      valid+=1
      if best is None or z[0]>best[0]:best=z
      assert frozen_signature(z[2])==frozen_signature(init)
      assert abs(wealth(z[2],db,dq)-wealth(init,db,dq)+z[0])<1e-7
    rows.append(dict(amp=amp,db=db,dq=dq,kind=KINDS[k],mode=mode,valid=valid,rejected=sum(err.values()),best_profit='' if best is None else best[0],best_atoms='' if best is None else best[1],best_direction='' if best is None else best[5],error=json.dumps(dict(err))))
    print('fixed grid',amp,db,dq,k,mode,'best',None if best is None else best[0],flush=True)
    r.close()
 dump('candidate_fix_search',rows);return rows

def forced_cycles():
 rows=[]
 for fixed in [False,True]:
  for k,mode,raw in [(3,'native',4200*10**9),(4,'native',4200*10**9),(2,'bounded',10*10**9),(2,'blend',10*10**9)]:
   for recover in ([False] if k in [3,4] else [False,True]):
    r=RPC(fix_frozen=fixed,fix_carry=fixed and mode!='native');a=Research(r,4,mode);s=r(op='new',kind=k,amp=4,db=9,dq=6);init=s;cum=0;intsum=0
    for cycle in range(1,33):
     if recover and k==2 and cycle%4==1:
      before=wealth(s,9,6);v=r(op='advance',id=s['id'],slot=500000+cycle*9000)
      assert 'error' not in v,v;s=v;intsum+=wealth(s,9,6)-before
     # Every fourth cycle permits funding to be paid once. Others retain it.
     carry=k==2 and (not recover or cycle%4!=0)
     d=1 if k==4 else 0;amount=raw*60//1000 if d==1 else raw
     z,err=roundtrip(a,s,d,amount,9,6,carry)
     if z is None:
      rows.append(dict(fixed=fixed,kind=KINDS[k],mode=mode,recovery=recover,cycle=cycle,status=err,profit='',cumulative=cum,lp_trade_loss='',funding_due_quote='',interest_paid_quote='',frozen_mask=s['meta'][9]));break
     profit,atoms,w,v,amount,d=z;cum+=profit;s=w
     assert abs(wealth(s,9,6)-wealth(init,9,6)-intsum+cum)<1e-7
     if k in [3,4]:assert frozen_signature(s)==frozen_signature(init)
     rows.append(dict(fixed=fixed,kind=KINDS[k],mode=mode,recovery=recover,cycle=cycle,status='accepted',profit=profit,cumulative=cum,lp_trade_loss=wealth(init,9,6)+intsum-wealth(s,9,6),funding_due_quote=(s['meta'][5]-s['meta'][7])/1e6,interest_paid_quote=s['books'][3]/1e6,frozen_mask=s['meta'][9]))
    r.close()
 dump('candidate_fix_cycles',rows);return rows

def differential_fixed():
 rows=[]
 for ff,fc in [(False,False),(True,False),(True,True)]:
  r=RPC(fix_frozen=ff,fix_carry=fc)
  for amp in [1,4]:
   for db,dq in [(9,6),(6,9),(2,2),(0,0)]:
    for k in range(6):
     s=r(op='new',kind=k,amp=amp,db=db,dq=dq)
     for d in [0,1]:
      for frac in [0,.001,.2]:
       v=r(s=rpcstate(s['s']),amount=str(int(s['s'][d]*frac)),direction=d,amp=amp)
       if 'error' in v:rows.append(dict(frozen_fix=ff,carry_fix=fc,amp=amp,db=db,dq=dq,kind=k,d=d,frac=frac,result=brief(v),delta=''));continue
       p,e=reconstruct(v['s'],ff,fc);delta=max(abs(x-y) for x,y in zip(p+e,v['point']+v['endpoint']));assert delta==0
       rows.append(dict(frozen_fix=ff,carry_fix=fc,amp=amp,db=db,dq=dq,kind=k,d=d,frac=frac,result='exact',delta=delta))
  r.close()
 dump('candidate_integer_differential',rows);return rows

if __name__=='__main__':
 for f in [optimized_fixed,forced_cycles,differential_fixed]:
  result=f();print(f.__name__,len(result),flush=True)
