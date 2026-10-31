
import os,json,math,csv
from pathlib import Path
import numpy as np
from numpy.polynomial.legendre import leggauss
OUT=Path('analysis/op-gamm-stateful-2026-10-27/results')
OUT.mkdir(parents=True,exist_ok=True)
C=60.0
H=.85*.95
class Shape:
 def __init__(self,A=1,core=.1,outer=.5):
  self.A=A
  self.layers=[]
  if A>1:
   avg=.5*(1/math.sqrt(1+core)+1/math.sqrt(1+outer))
   s=(1-1/A)/avg
   assert 0<s<1
   self.layers=[(s/(2*(1-s)),1/math.sqrt(1+w),math.sqrt(1+w)) for w in (core,outer)]
  self.breaks=[math.log(v) for w,a,b in self.layers for v in (a,b)]
 def xyh(self,t):
  t=np.asarray(t);u=np.exp(t)
  X=1/u;Y=u.copy();active=np.ones_like(u)
  for w,a,b in self.layers:
   X=X+w*np.where(u<=a,1/a-1/b,np.where(u<b,1/u-1/b,0))
   Y=Y+w*np.where(u<=a,0,np.where(u<b,u-a,b-a))
   active=active+w*((u>a)&(u<b))
  H=active*(u/Y+1/(u*X))
  return X,Y,H
 def t_for_r(self,r):
  r=float(r)
  if not self.layers:return .5*math.log(r)
  points=[0.0]+sorted({v for w,a,b in self.layers for v in (a,b)})+[float('inf')]
  for a,b in zip(points[:-1],points[1:]):
   mid=b/2 if a==0 else (a*2 if math.isinf(b) else math.sqrt(a*b))
   A=1.;B=0.;D=0.
   for w,lo,hi in self.layers:
    if mid<lo:B+=w*(1/lo-1/hi)
    elif mid>hi:D+=w*(hi-lo)
    else:A+=w;B-=w/hi;D-=w*lo
   v=D-r*B
   disc=math.sqrt(v*v+4*r*A*A)
   u=2*r*A/(v+disc) if v>0 else (-v+disc)/(2*A)
   if a-1e-12<=u<=b+1e-12:return math.log(u)
  raise ValueError(r)
 def tail(self,x,y,center=C):
  t=self.t_for_r(y/(center*x));X,Y,_=self.xyh(t)
  return float(x*math.sqrt(center)/X)
class PathCurve:
 def __init__(self,shape,lam=0,T=None,O=C,center=C,order=12,step=.01):
  self.s=shape;self.lam=lam;self.T=T;self.center=center
  self.pi=0. if lam==0 else math.log(O/center)
  self.B=math.inf if T is None else math.log(T)
  extra=shape.breaks+[0.]
  if lam and T is not None:extra +=[(self.pi-self.B)/2,(self.pi+self.B)/2]
  self.ts=np.unique(np.r_[np.arange(-25,25+step/2,step),extra])
  self.z,self.w=leggauss(order)
  a=self.ts[:-1];b=self.ts[1:]
  ints=self.area(a,b)
  self.gs=np.r_[0.,np.cumsum(ints)]
 def p(self,t):
  t=np.asarray(t)
  if self.lam==0:return self.center*np.exp(2*t)
  d=np.clip(self.pi-2*t,-self.B,self.B)
  return self.center*np.exp(2*t+self.lam*d)
 def f(self,t):
  X,Y,H0=self.s.xyh(t);r=Y/X
  return H0*r/(r+self.p(t)/self.center)
 def area(self,a,b):
  a=np.asarray(a);b=np.asarray(b)
  m=(a+b)/2;h=(b-a)/2
  return h*np.sum(self.f(m[...,None]+h[...,None]*self.z)*self.w,axis=-1)
 def G(self,t):
  t=np.asarray(t)
  j=np.clip(np.searchsorted(self.ts,t,side='right')-1,0,len(self.ts)-2)
  return self.gs[j]+self.area(self.ts[j],t)
 def invG(self,target):
  target=np.asarray(target)
  t=np.interp(target,self.gs,self.ts)
  for _ in range(3):
   f=self.f(t);t=t-(self.G(t)-target)/np.maximum(f,1e-25)
  return t
 def endpoint_x(self,x,y,newx):
  newx=np.asarray(newx)
  t0=self.s.t_for_r(y/(self.center*x))
  target=self.G(t0)-np.log(newx/x)
  valid=target>=self.gs[0]+1e-12
  t=self.invG(np.maximum(target,self.gs[0]+1e-12))
  X,Y,_=self.s.xyh(t)
  newy=self.center*newx*(Y/X)
  newy=np.where(valid,newy,0.)
  return newy,t
 def state_at_t(self,x,y,t):
  t0=self.s.t_for_r(y/(self.center*x))
  newx=x*np.exp(self.G(t0)-self.G(t))
  X,Y,_=self.s.xyh(t)
  return float(newx),float(self.center*newx*Y/X)
 def t_at_price(self,p):
  lo,hi=-25.,25.
  for _ in range(80):
   mid=(lo+hi)/2
   if self.p(mid)<p:lo=mid
   else:hi=mid
  return (lo+hi)/2
 def local_depth(self,x,y,S=C):
  t=self.s.t_for_r(y/(self.center*x))
  X,Y,H0=self.s.xyh(t)
  beta0=2/float(H0)
  inside=self.lam>0 and abs(self.pi-2*t)<self.B
  beta=beta0*(1-self.lam if inside else 1)
  p=float(self.p(t))
  return S*x/(beta*(1+p/(y/x))) # notional for one unit log-price movement
def writecsv(name,rows):
 if not rows:return
 with (OUT/(name+'.csv')).open('w') as f:
  wr=csv.DictWriter(f,fieldnames=list(rows[0]));wr.writeheader();wr.writerows(rows)
def credit(P,L,c,E=0):
 xs=L/math.sqrt(P);ys=L*math.sqrt(P)
 v=ys*np.asarray(c)/(xs+np.asarray(c))
 if E>=ys:return np.zeros_like(v)
 u=xs*E/(ys-E)
 marginal=ys*(u+np.asarray(c))/(xs+u+np.asarray(c))-E
 return .95*np.minimum(.85*v,np.maximum(0,marginal))
def native_global_draw(P,L,c,existing,stored_contribution,cash,health_floor=1.1):
 xs=L/math.sqrt(P);ys=L*math.sqrt(P)
 upper=min(cash,H*ys*c/(xs+c),max(0,ys-existing)*(1-1e-12))
 def ok(draw):
  total=existing+draw
  contrib=min(stored_contribution+min(c,1.5*draw/P),1.5*total/P)
  required=xs*total/(ys-total) if total<ys else math.inf
  E=existing*min(1,required/contrib) if contrib>0 else existing
  return draw<=credit(P,L,c,E)+1e-10 and contrib>=health_floor*required
 lo=0.;hi=max(0,upper)
 for _ in range(65):
  mid=(lo+hi)/2
  if ok(mid):lo=mid
  else:hi=mid
 return lo,min(c,1.5*lo/P)
def name(lam,T):
 return 'native' if lam==0 else ('blend' if T is None else 'bounded_'+str(T))
SHAPES={'cpmm':Shape(1),'dusk_4x':Shape(4)}

def single_credit(P,L,c,cash):
 ys=L*math.sqrt(P)
 return min(cash,float(credit(P,L,c)),ys*(1-1.1/1.5))
def optimal_single(P,L,S,cash,cmax):
 xs=L/math.sqrt(P);ys=L*math.sqrt(P)
 cap=min(cash,ys*(1-1.1/1.5))
 c=max(0,min(cmax,xs*(math.sqrt(H*P/S)-1)))
 if H*ys>cap:c=min(c,xs*cap/(H*ys-cap))
 return c
