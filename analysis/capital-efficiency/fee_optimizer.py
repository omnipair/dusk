"""Solve fixed-state arbitrage across uncapped and capped outward-fee regimes.

The outward potential is convex in gross input before its proportional cap.
The profit derivative is decreasing within each regime; a fee-cap transition
can create a second local maximum. Check both regimes and their boundary.
"""
import math
from dynamic_fees import potential


def optimal_trade(market,start,external,acc,config,iterations=70):
    buy=external>=start
    base=config.base_bps/10000
    pressure=config.volatility_coefficient*acc
    vol=min((1-base)*pressure/(1+pressure),config.volatility_cap_bps/10000,.5-base)
    alpha=1-base-vol
    cap=min(config.divergence_cap_bps/10000,.5-base-vol)
    empty=market.trade(0,start,external,acc,config,buy)
    if max(start/external,external/start)*alpha<=1+1e-13:
        return empty
    p0,pe=market.point(start),market.point(external)
    rin=p0[1] if buy else p0[0]
    net_to_external=abs((pe[1]-p0[1]) if buy else (pe[0]-p0[0]))
    upper=2*net_to_external
    if upper<1e-10: return empty
    center=market.center_input
    coefficient=config.divergence_coefficient
    u0=max(0,rin-center)
    f0=potential(u0,center,coefficient)
    def slope(u):
        if u<=0 or coefficient==0: return 0.0
        return 4*coefficient*u*u*(3*center+2*u)/(3*center*(center+u)**2)
    def fee_difference(g):
        return potential(max(0,rin+alpha*g-center),center,coefficient)-f0
    def cap_excess(g):
        return fee_difference(g)-cap*g
    regimes=[]
    if coefficient==0 or cap==0:
        regimes=[(0.0,upper,False)]
        # A zero cap switches off the outward fee entirely.
        if cap==0: coefficient=0.0;f0=0.0
    elif alpha*slope(u0)>=cap:
        regimes=[(0.0,upper,True)]
    elif cap_excess(upper)<=0:
        regimes=[(0.0,upper,False)]
    else:
        lo,hi=0.0,upper
        for _ in range(iterations):
            mid=(lo+hi)/2
            if cap_excess(mid)>0: hi=mid
            else: lo=mid
        boundary=(lo+hi)/2
        regimes=[(0.0,boundary,False),(boundary,upper,True)]
    def derivative(g,capped):
        u=max(0,rin+alpha*g-center)
        fee=cap*g if capped else fee_difference(g)
        net=alpha*g-fee
        net_derivative=alpha-cap if capped else alpha*(1-slope(u))
        inverse_price=market.price_at_quote(rin+net)
        end=inverse_price if buy else 1/inverse_price
        return (external/end if buy else end/external)*net_derivative-1
    candidates={0.0,upper}
    for lo,hi,capped in regimes:
        candidates.update([lo,hi])
        if derivative(lo,capped)<=0 or derivative(hi,capped)>=0:
            continue
        for _ in range(iterations):
            mid=(lo+hi)/2
            if derivative(mid,capped)>0: lo=mid
            else: hi=mid
        candidates.add((lo+hi)/2)
    trades=[market.trade(g,start,external,acc,config,buy) for g in candidates]
    best=max(trades,key=lambda t:t['arb_profit_usd'])
    return best if best['arb_profit_usd']>1e-8 else empty
