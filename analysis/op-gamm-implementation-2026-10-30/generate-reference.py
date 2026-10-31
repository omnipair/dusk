"""Independent 80-digit checks; mpmath==1.3.0, Python >=3.9.

Builds native homothetic inventory directly, inverts its ratio by bisection,
and integrates with mpmath quadrature. It does not use Rust's quadratic inverse,
fixed-point transcendental series, or Gauss-2/Gauss-4 remainder implementation.
"""
import csv
import math
from pathlib import Path
import mpmath as mp

mp.mp.dps = 80
NAD, Q = 10**9, 1 << 64
HERE = Path(__file__).resolve().parent

def shape(amp, core, fade):
    if amp == 1:
        return []
    widths = [core] if fade == 0 else [core, core + fade]
    highs = [math.isqrt((NAD + w*NAD//10000)*NAD) for w in widths]
    avg_inverse = sum(NAD*NAD//h for h in highs)//len(highs)
    concentrated = (NAD-NAD//amp)*NAD//avg_inverse
    tail = NAD-concentrated
    assert tail > 0
    weight = mp.mpf(concentrated)/tail/len(highs)
    return [(weight, mp.mpf(NAD)/h, mp.mpf(h)/NAD) for h in highs]

def coefficients(layers, t):
    u=mp.exp(t)
    a,b,d=mp.mpf(1),mp.mpf(0),mp.mpf(0)
    for w,lo,hi in layers:
        if u < lo: b += w*(1/lo-1/hi)
        elif u > hi: d += w*(hi-lo)
        else: a,b,d = a+w,b-w/hi,d-w*lo
    return a,b,d

def coordinate(layers, ratio):
    low,high=mp.mpf(-32),mp.mpf(32)
    for _ in range(270):
        t=(low+high)/2
        u=mp.exp(t)
        a,b,d=coefficients(layers,t)
        current=(a*u+d)/(a/u+b)
        if current<ratio: low=t
        else: high=t
    return (low+high)/2

def reference(amp,core,fade,lam,oracle,cap,start,end):
    layers=shape(amp,core,fade)
    lam=mp.mpf(lam)/10000
    oracle=mp.mpf(oracle)/60
    cap=mp.mpf(cap)/NAD
    ta=coordinate(layers,mp.mpf(start[1])/start[0]/60)
    tb=coordinate(layers,mp.mpf(end[1])/end[0]/60)
    def price(t):
        native=2*t
        adjustment=max(-mp.log(cap),min(mp.log(cap),lam*(mp.log(oracle)-native)))
        return mp.exp(native+adjustment)
    def integrand(t):
        u=mp.exp(t)
        a,b,d=coefficients(layers,t)
        x,y=a/u+b,a*u+d
        return a*(u*x+y/u)/(x*(y+price(t)*x))
    cuts=[mp.log(v) for _,lo,hi in layers for v in [lo,hi]]
    cuts += [(mp.log(oracle)+sign*mp.log(cap)/lam)/2 for sign in [-1,1]]
    left,right=sorted([ta,tb])
    cuts=sorted(set([left,right]+[v for v in cuts if left<v<right]))
    integral=sum(mp.quad(integrand,[a,b]) for a,b in zip(cuts,cuts[1:]))
    if ta>tb: integral=-integral
    delta=mp.log(mp.mpf(end[0])/start[0])+integral
    return int(mp.floor(delta*Q)),int(mp.floor(price(tb)*60*NAD))

def main():
    with (HERE/'transcendental-reference.csv').open('w') as f:
        out=csv.writer(f);out.writerow(['input_q64','ln_floor_q64','exp_floor_q64'])
        values=[1,2,3,Q-1,Q,Q+1,2*Q-1,2*Q,2*Q+1,(1<<126)-1]
        values += [int(mp.exp(mp.mpf(i)/5)*Q) for i in range(-200,201,7)]
        for value in values:
            x=mp.mpf(value)/Q
            exp_floor=int(mp.floor(mp.exp(x)*Q)) if x<=40 else -1
            out.writerow([value,int(mp.floor(mp.log(x)*Q)),exp_floor])
    with (HERE/'invariant-reference.csv').open('w') as f:
        out=csv.writer(f)
        out.writerow(['amp','core','fade','lambda','oracle','cap','x0','y0','x1','y1','delta_floor_q64','price_floor_nad'])
        start=(10**10,600*10**9)
        cases=0
        for amp,core,fade in [(1,0,0),(4,1000,4000),(32,100,0),(2000,1,0)]:
            for lam in [1,2500,5000,9999]:
                for oracle in [6,60,600]:
                    for end in [(start[0]*10001//10000,start[1]*9999//10000),
                                (start[0]*2,start[1]//2),
                                (start[0]//2,start[1]*2)]:
                        row=[amp,core,fade,lam,oracle,2*NAD,*start,*end]
                        out.writerow(row+list(reference(amp,core,fade,lam,oracle,2*NAD,start,end)))
                        cases+=1
                        f.flush()
        print(f'Wrote {cases} invariant references at {mp.mp.dps} decimal digits.')

if __name__=='__main__':main()
