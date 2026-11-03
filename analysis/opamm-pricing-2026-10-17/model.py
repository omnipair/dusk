"""Independent analytic OP-AMM reference model; no Dusk program integration.

X = launch_price * base_reserve and Y = quote_reserve are quote-value units.
The dimensionless oracle ratio is S = reference_price / launch_price.
For a rule with oracle weight lam and reserve elasticity beta:

    marginal_price / launch_price = S**lam * (Y/X)**beta.

PT uses (lam, beta) = (1, 1). GA with a CPMM baseline and lam=1/2
uses (1/2, 1/2). A depth-matched GA uses (1/2, 1), corresponding to
the valid baseline phi(r)=r**2. The latter isolates oracle sensitivity.
"""

from __future__ import annotations

from dataclasses import dataclass
from math import expm1, isfinite, log, log1p


@dataclass(frozen=True)
class Rule:
    name: str
    lam: float
    beta: float

    def validate(self) -> None:
        if not (0 <= self.lam <= 1 and 0 < self.beta <= 1):
            raise ValueError("invalid rule")


RULES = (
    Rule("PT_full", 1.0, 1.0),
    Rule("GA_25_matched", 0.25, 1.0),
    Rule("GA_50_matched", 0.5, 1.0),
    Rule("GA_75_matched", 0.75, 1.0),
    Rule("GA_50_CPMM", 0.5, 0.5),
    Rule("CPMM_no_oracle", 0.0, 1.0),
    # Approximate Dusk's 5x center depth by matching the launch-price slope.
    # These are power-law research curves, not Dusk's five-segment executable curve.
    Rule("PT_full_local5x", 1.0, 0.2),
    Rule("GA_50_local5x", 0.5, 0.2),
    Rule("No_oracle_local5x", 0.0, 0.2),
)


@dataclass
class Pool:
    launch_price: float
    base: float
    quote: float
    rule: Rule
    fee: float = 0.0003

    def __post_init__(self) -> None:
        self.rule.validate()
        if not (self.launch_price > 0 and self.base > 0 and self.quote > 0 and 0 <= self.fee < 1):
            raise ValueError("invalid pool")

    def fork(self) -> Pool:
        return Pool(self.launch_price, self.base, self.quote, self.rule, self.fee)

    def _xy_a(self, reference: float) -> tuple[float, float, float]:
        if not (reference > 0 and isfinite(reference)):
            raise ValueError("invalid reference")
        x = self.launch_price * self.base
        y = self.quote
        a = (reference / self.launch_price) ** self.rule.lam
        return x, y, a

    def marginal_price(self, reference: float) -> float:
        x, y, a = self._xy_a(reference)
        return self.launch_price * a * (y / x) ** self.rule.beta

    def invariant(self, reference: float) -> float:
        """A monotone transform of the conditional invariant at fixed reference."""
        x, y, a = self._xy_a(reference)
        if self.rule.beta == 1:
            return a * log(x) + log(y)
        c = 1 - self.rule.beta
        return y**c + a * x**c

    def buy_base_exact_in(self, gross_quote: float, reference: float) -> float:
        """Quote in, base out. The fee stays in the quote reserve."""
        if gross_quote <= 0:
            raise ValueError("quote input must be positive")
        x, y, a = self._xy_a(reference)
        net = gross_quote * (1 - self.fee)
        if self.rule.beta == 1:
            out_x = -x * expm1(-log1p(net / y) / a)
        else:
            c = 1 - self.rule.beta
            remaining = y**c + a * x**c - (y + net) ** c
            if remaining <= 0:
                raise ValueError("insufficient base reserve")
            out_x = x - (remaining / a) ** (1 / c)
        out = out_x / self.launch_price
        if not (0 < out < self.base and isfinite(out)):
            raise ValueError("insufficient base reserve")
        self.base -= out
        self.quote += gross_quote
        return out

    def sell_base_exact_in(self, gross_base: float, reference: float) -> float:
        """Base in, quote out. The fee stays in the base reserve."""
        if gross_base <= 0:
            raise ValueError("base input must be positive")
        x, y, a = self._xy_a(reference)
        net_x = self.launch_price * gross_base * (1 - self.fee)
        if self.rule.beta == 1:
            out_y = -y * expm1(-a * log1p(net_x / x))
        else:
            c = 1 - self.rule.beta
            remaining = y**c + a * x**c - a * (x + net_x) ** c
            if remaining <= 0:
                raise ValueError("insufficient quote reserve")
            out_y = y - remaining ** (1 / c)
        if not (0 < out_y < self.quote and isfinite(out_y)):
            raise ValueError("insufficient quote reserve")
        self.base += gross_base
        self.quote -= out_y
        return out_y

    def buy_base_exact_out(self, base_out: float, reference: float) -> float:
        """Base out, gross quote in. Used to make attacker cycles base-neutral."""
        if not (0 < base_out < self.base):
            raise ValueError("invalid base output")
        x, y, a = self._xy_a(reference)
        target_x = x - self.launch_price * base_out
        if self.rule.beta == 1:
            net = y * expm1(a * log1p((x - target_x) / target_x))
        else:
            c = 1 - self.rule.beta
            target_y_c = y**c + a * x**c - a * target_x**c
            net = target_y_c ** (1 / c) - y
        gross = net / (1 - self.fee)
        if not (gross > 0 and isfinite(gross)):
            raise ValueError("invalid quote requirement")
        self.base -= base_out
        self.quote += gross
        return gross

    def nav(self, mark: float) -> float:
        return self.base * mark + self.quote
