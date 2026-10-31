from run_cycles import *
def checks():
 rows=[];r0=RPC();rf=RPC(fix_frozen=True)
 for amp in [1,4]:
  for db,dq in [(9,6),(6,9),(2,2),(0,0)]:
   for k in [0,1]:
    s0=r0(op='new',kind=k,amp=amp,db=db,dq=dq);sf=rf(op='new',kind=k,amp=amp,db=db,dq=dq)
    a0=Research(r0,amp,'native');af=Research(rf,amp,'native')
    for d in [0,1]:
     for raw in sizes(s0,d,db,dq)[::2]:
      x=a0.quote(s0,d,raw);y=af.quote(sf,d,raw)
      if 'error' in x or 'error' in y:assert brief(x)==brief(y),(x,y)
      else:
       for key in ['out','meta','books','point','s']:assert x.get(key)==y.get(key),(key,x,y)
      rows.append(dict(check='unfrozen native unchanged',amp=amp,db=db,dq=dq,kind=k,d=d,raw=raw,status='pass'))
 r0.close();rf.close()
 for fixed in [False,True]:
  r=RPC(fix_frozen=fixed)
  for k in range(6):
   s=r(op='new',kind=k,amp=4,db=9,dq=6)
   a=Research(r,4,'native')
   for d in [0,1]:
    for raw in [10**6,10**10] if d==0 else [1000,600000000]:
     control=a.quote(s,d,raw)
     for mode in ['blend','bounded']:
      for O in [None,0,-1,10**100]:
       z=Research(r,4,mode,lam=0).quote(s,d,raw,O)
       if 'error' in control or 'error' in z:assert brief(control)==brief(z)
       else:
        for key in ['out','meta','books','point','s']:assert control.get(key)==z.get(key)
       rows.append(dict(check='lambda zero no oracle '+str(fixed),amp=4,db=9,dq=6,kind=k,d=d,raw=raw,status='pass'))
  r.close()
 dump('regressions',rows);return rows

def tiny_rounding():
 rows=[]
 for fixed in [False,True]:
  r=RPC(fix_frozen=fixed);a=Research(r,1,'native');s=r(op='new',kind=1,amp=1,db=6,dq=9);cum=0
  for cycle in range(1,65):
   best=None
   for d in [0,1]:
    for raw in sizes(s,d,6,9):
     z,e=roundtrip(a,s,d,raw,6,9,False)
     if z is not None and (best is None or z[0]>best[0]):best=z
   if best is None:break
   rows.append(dict(fixed=fixed,cycle=cycle,best_profit=best[0],atoms=best[1],direction=best[5],input_atoms=best[4],cumulative=cum+max(0,best[0])))
   if best[0]<=0:break
   cum+=best[0];s=best[2]
  r.close()
 dump('unchanged_rounding_residual',rows);return rows
def cash_constrained_fixed():
 rows=[]
 for mode in ['native','blend','bounded']:
  r=RPC(fix_frozen=True,fix_carry=mode!='native');a=Research(r,4,mode);s=r(op='new',kind=6,amp=4,db=9,dq=6)
  for d in [0,1]:
   for n in [1,10,100,1000,10000]:
    z,e=roundtrip(a,s,d,n,9,6,True)
    if z is not None:assert z[0]<=0
    rows.append(dict(mode=mode,d=d,input_atoms=n,status=e or 'accepted',profit='' if z is None else z[0],frozen_mask=s['meta'][9]))
  r.close()
 dump('cash_constrained_carry_fixed',rows);return rows
if __name__=='__main__':
 print('regressions',len(checks()),flush=True);print('rounding',tiny_rounding(),flush=True);print('cash constrained',len(cash_constrained_fixed()),flush=True)
