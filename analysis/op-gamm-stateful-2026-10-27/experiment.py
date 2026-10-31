"""Research: pinned native ledger, shadow oracle conditional invariant. No production integration."""
import json, math, subprocess, time, csv, collections
from pathlib import Path
import numpy as np
from math_reference import Shape, PathCurve
ROOT=Path(__file__).resolve().parent
REPO=ROOT.parents[1]
OUT=ROOT/'results';OUT.mkdir(exist_ok=True)
class RPC:
 def __init__(self,fix_frozen=False,fix_carry=False):
  self.fix_frozen=fix_frozen;self.fix_carry=fix_carry
  self.p=subprocess.Popen([str(REPO/'target/debug/dusk-stateful-oracle-research')],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True,bufsize=1)
  self.calls=0
  self(op="fixes",frozen=fix_frozen,carry=fix_carry)
 def __call__(self,**q):
  self.calls+=1;self.p.stdin.write(json.dumps(q)+'\n');self.p.stdin.flush()
  while True:
   line=self.p.stdout.readline()
   if not line:raise RuntimeError('native process ended')
   if line.startswith('{'):break
  r=json.loads(line)
  for k in ['s','point','books','endpoint']:
   if k in r:r[k]=[int(x) for x in r[k]]
  if 'out' in r:r['out']=int(r['out'])
  return r
 def close(self):self.p.terminate();self.p.wait()
def rpcstate(s):return [str(x) for x in s]
def signed_mul(v,n,d):return (-1 if v<0 else 1)*(abs(v)*n//d)
def reconstruct(s,fix_frozen=False,fix_carry=False):
 a,b=s[0]+s[8],s[1]+s[9];eb,eq=s[2:4]
 if min(a,b)<=0:raise ValueError('book inventory')
 ha=a+(s[5] if fix_frozen and s[14]&2 else 0)+(s[7]*s[12] if fix_carry else 0)
 hb=b+(s[4] if fix_frozen and s[14]&1 else 0)+(s[6]*s[13] if fix_carry else 0)
 dq=s[4] if s[14]&1 else eb*hb//ha+s[6]*s[13]
 db=s[5] if s[14]&2 else eq*ha//hb+s[7]*s[12]
 x,y=a+eb+db-s[10],b+eq+dq-s[11]
 if min(x,y)<=0:raise ValueError('live inventory')
 bl,ql=x+s[10],y+s[11]
 ebasis=eb*x//bl+signed_mul(eb*y//bl-dq,x,y)
 eqbasis=eq*y//ql+signed_mul(eq*x//ql-db,y,x)
 if fix_frozen and s[14]&1:ebasis=0
 if fix_frozen and s[14]&2:eqbasis=0
 p=[x-ebasis-db+s[15],y-eqbasis-dq+s[16]]
 if min(p)<=0:raise ValueError('curve inventory')
 return p,[x,y,dq,db]
def endpoint(s,amount,out,d,fix_frozen=False,fix_carry=False):
 e=s.copy();e[d]+=amount;e[1-d]-=out
 p,v=reconstruct(e,fix_frozen,fix_carry);e[4:6]=v[2:4];return e,p
class Research:
 def __init__(self,rpc,amp=4,mode='bounded',lam=.5,M=2):
  self.rpc=rpc;self.shape=Shape(amp);self.amp=amp;self.mode=mode;self.lam=0 if mode=='native' else lam;self.M=M;self.cache={}
 def curve(self,O):
  key=60 if self.lam==0 else O
  if key not in self.cache:self.cache[key]=PathCurve(self.shape,self.lam,None if self.lam==0 or self.mode in ['native','blend'] else self.M**(1/self.lam),O=key)
  return self.cache[key]
 def U(self,p,c):
  x,y=p;t=self.shape.t_for_r(y/(60*x));return math.log(x)+float(c.G(t))
 def mark(self,p,c):return int(float(c.p(self.shape.t_for_r(p[1]/(60*p[0]))))*1e9)
 def quote(self,st,d,raw,O=60,commit=False,policy='materialized',carry=False):
  if raw<=0:return {'error':'zero input'}
  c=self.curve(O);mark=0 if self.lam==0 else self.mark(st['point'],c)
  start=self.rpc(op='stats',id=st['id'],mark=str(mark),carry=carry)
  if 's' not in start:return {'error':start.get('state_error','no state')}
  s=start['s'];
  atom=s[12+d];oatom=s[13-d];amount=raw*atom
  # Literal lambda-zero branch: complete native prepare/finalize ledger path.
  # O is unused; actual token CPIs are outside this host-native harness.
  if self.lam==0:
   v=self.rpc(op='native',id=st['id'],input=str(raw),direction=d,commit=commit)
   if 'error' not in v:v.update(rawin=raw,du=self.U(v['point'],c)-self.U(st['point'],c),certificate='native_full_ledger')
   return v
  def make_end(n,input_amount=amount):
   # Retained funding remains a real liability. Solve its target-asset cover
   # at the ENDPOINT mark, so the next quote cannot re-mark the same carry
   # and charge the hLP again. No cash or debt is erased by this adjustment.
   endmark=mark
   for _ in range(32):
    adjusted=s.copy()
    if self.rpc.fix_carry and carry:
     db=(s[6]*s[13]*10**9+endmark-1)//endmark if not s[14]&1 else 0
     dq=(s[7]*s[12]*endmark+10**9-1)//10**9 if not s[14]&2 else 0
     adjusted[2]+=db;adjusted[8]-=db;adjusted[3]+=dq;adjusted[9]-=dq
    e,p=endpoint(adjusted,input_amount,n*oatom,d,self.rpc.fix_frozen,self.rpc.fix_carry)
    wanted=self.mark(p,c)
    if not self.rpc.fix_carry or not carry or abs(wanted-endmark)<=1: return e,p
    endmark=wanted
   raise ValueError('carry fixed point did not converge')
  # Unrounded continuous reconstructed endpoint is the fast initial search.
  # Corrected variant also checks the actual rounded Rust ledger after settlement.
  target=self.U(st['point'],c)
  if self.rpc.fix_carry and carry:
   # Quote from the funded-accounting preparation as well as the pre-call
   # inventory. A no-trade ownership refresh is not a rebate to the next trader.
   try:zero,pzero=make_end(0,0)
   except ValueError as ex:return {'error':str(ex)}
   baseline=self.rpc(op='settle',id=st['id'],s=rpcstate(zero),input='0',out='0',direction=d,commit=False)
   if 'error' in baseline:return baseline
   target=max(target,self.U(pzero,c),self.U(baseline['point'],c))
  hi=min((min(s[1-d],s[1-d]+s[9-d])-1)//oatom,start['meta'][4-d]-1)
  if hi<=0:return {'error':'cash floor'}
  lo=0;best=None
  guard=3e-13 # > observed float U noise; final robustness rerun uses 10x.
  for _ in range(64):
   if lo>=hi:break
   mid=(lo+hi+1)//2
   try:
    e,p=make_end(mid)
    u=self.U(p,c) if policy!='cash_only' else self.U(e[:2],c)
    ok=u>=target+guard
   except (ValueError,OverflowError):ok=False
   if ok:lo=mid
   else:hi=mid-1
  out=lo
  if out<=0:return {'error':'zero output'}
  # Materialize at raw-token resolution. Errors reject the candidate; no state mutation.
  def trial(n,save=False):
   try:e,p=make_end(n)
   except ValueError as ex:return {'error':str(ex)}
   return self.rpc(op='settle',id=st['id'],s=rpcstate(e),input=str(raw),out=str(n),direction=d,commit=save)
  v=trial(out)
  if policy=='materialized':
   if 'error' in v:return v
   # Search downward only; never overpay based on numerical/integer discontinuities.
   if self.U(v['point'],c)<target+guard:
    lo,hi=0,out-1;best=None
    for _ in range(64):
     if lo>hi:break
     mid=(lo+hi)//2;w=trial(mid)
     ok='error' not in w and self.U(w['point'],c)>=target+guard
     if ok:best=(mid,w);lo=mid+1
     else:hi=mid-1
    if best is None or best[0]==0:return {'error':'materialized certificate unavailable'}
    out,v=best
  if 'error' in v:return v
  if commit:v=trial(out,True)
  if 'error' not in v:v.update(out=out,rawin=raw,du=self.U(v['point'],c)-target,certificate=policy)
  return v

def dump(name,rows):
 if not rows:return
 with (OUT/(name+'.csv')).open('w') as f:
  w=csv.DictWriter(f,fieldnames=list(rows[0]));w.writeheader();w.writerows(rows)
def validate():
 r=RPC();rows=[];fixtures=[]
 for amp in [1,4]:
  for db,dq in [(9,6),(6,9),(6,6),(2,2),(0,0)]:
   for kind in range(6):
    s=r(op='new',kind=kind,db=db,dq=dq,amp=amp);s.update(amp=amp,db=db,dq=dq,kind=kind);fixtures.append(s)
    if 'error' in s or 's' not in s:continue
    for d in [0,1]:
     for frac in [0,1e-8,.001,.1,.5]:
      a=int(frac*s['s'][d]);v=r(s=rpcstate(s['s']),amount=str(a),direction=d,amp=amp)
      if 'error' in v:
       rows.append(dict(amp=amp,db=db,dq=dq,kind=kind,d=d,frac=frac,status='native_reject',delta='',error=v['error']));continue
      p,e=reconstruct(v['s']);delta=max(abs(x-y) for x,y in zip(p+e,v['point']+v['endpoint']))
      assert delta==0
      rows.append(dict(amp=amp,db=db,dq=dq,kind=kind,d=d,frac=frac,status='exact',delta=delta,error=''))
 (OUT/'fixtures.json').write_text(json.dumps(fixtures,indent=2));dump('integer_kernel_differential',rows);r.close();return rows
if __name__=='__main__':
 rows=validate();print(collections.Counter(x['status'] for x in rows))
