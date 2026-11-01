"""Mathematical reference for the V1 band/half-life controller with risk input.

This checks the economic contract, not native fixed-point/Solana execution. It
deliberately does not reproduce V1's truncated integer exp_rate coefficient.
"""
from dataclasses import dataclass
from math import exp, expm1, isfinite, log

DAY_MS=86_400_000


@dataclass(frozen=True)
class RateModel:
    target_start: float=.05
    target_end: float=.0625
    half_life_ms: int=14*DAY_MS
    initial_rate: float=.04
    min_rate: float=.02
    max_rate: float=0.  # V1 convention: no configured economic ceiling

    @classmethod
    def from_overrides(cls, **overrides):
        """Missing/None inputs use defaults; explicit zero values are preserved."""
        return cls(**{name:value for name,value in overrides.items() if value is not None})

    def __post_init__(self):
        assert 0<self.target_start<self.target_end
        assert all(isfinite(x) for x in (self.target_start,self.target_end,
            self.initial_rate,self.min_rate,self.max_rate))
        assert 3_600_000<=self.half_life_ms<=30*DAY_MS
        assert 0<=self.min_rate<=self.initial_rate and self.initial_rate>0
        assert self.max_rate==0 or self.max_rate>=self.initial_rate

    def integrate(self, last_rate, signal, elapsed_ms):
        """Return current APR and rate*time, with time measured in milliseconds."""
        assert elapsed_ms>=0 and isfinite(last_rate) and last_rate>=0
        assert signal is None or (signal>=0 and isfinite(signal))
        if elapsed_ms==0:
            return last_rate,0.
        start=max(last_rate,self.min_rate)
        if self.max_rate:
            start=min(start,self.max_rate)
        # Coverage is the inverse of V1 utilization pressure: low coverage raises rates.
        direction=0 if signal is None else 1 if signal<self.target_start else -1 if signal>self.target_end else 0
        if direction==0 or start==0:
            return start,start*elapsed_ms
        k=direction*log(2)/self.half_life_ms
        bound=self.max_rate if direction>0 else self.min_rate
        bounded=bool(self.max_rate) if direction>0 else self.min_rate>0
        crossing=max(0.,log(bound/start)/k) if bounded else float("inf")
        exponential_time=min(elapsed_ms,crossing)
        delta=start*expm1(k*exponential_time)
        end=start+delta
        integral=delta/k
        if exponential_time<elapsed_ms:
            end=bound
            integral+=bound*(elapsed_ms-exponential_time)
        return end,integral


class MarketController:
    """Authority-checked, prospective model replacement in a two-asset reference."""
    def __init__(self, authority, model, now_ms=0, signals=(None,None)):
        self.authority=authority
        self.model=model
        self.rates=[model.initial_rate,model.initial_rate]
        self.signals=signals
        self.last_ms=now_ms
        self.integrals=[0.,0.]

    def accrue(self, now_ms):
        assert now_ms>=self.last_ms
        results=[self.model.integrate(rate,score,now_ms-self.last_ms)
                 for rate,score in zip(self.rates,self.signals)]
        # Only change state after both calculations succeed.
        self.rates=[x[0] for x in results]
        self.integrals=[old+x[1] for old,x in zip(self.integrals,results)]
        self.last_ms=now_ms

    def set_model(self, signer, model, now_ms):
        assert signer==self.authority,"governance authority required"
        self.accrue(now_ms)  # settle old interval with old model before replacement
        self.model=model  # new initial_rate does not reset an existing market's APR

    def set_signals(self, signals, now_ms):
        assert len(signals)==2
        assert all(s is None or (s>=0 and isfinite(s)) for s in signals)
        self.accrue(now_ms)
        self.signals=signals


def band_at_reference_utilization(junior_start, junior_end, reference_utilization):
    """One-asset initialization helper; never recompute this band from live U."""
    assert 0<reference_utilization<1 and 0<junior_start<junior_end<=1
    factor=(1-reference_utilization)/reference_utilization
    return junior_start*factor,junior_end*factor
