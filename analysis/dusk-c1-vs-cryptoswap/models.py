"""Clean-room analytical models for the Dusk C1 decision study.

The common-coordinate level set is represented by two dimensionless values:

``v = |x-y| / (2*sqrt(x*y))`` and ``q(v) = 4*x*y / D**2``.

For two assets, the published CryptoSwap equation and Dusk's current inner
equation are identical under ``peak_depth = 2*A`` and
``fade_scale = gamma``.  The models below therefore share the same inner
curve.  They differ only after Dusk starts its protocol-fixed convergence to
an exact constant-product tail.

This is reference code for analysis.  It is deliberately independent of the
production fixed-point implementation and does not copy Curve source code.
"""

from __future__ import annotations

from dataclasses import dataclass
import math
from typing import ClassVar, Literal


EPSILON = 1e-14
MAX_ROOT_ITERATIONS = 96


class ArithmeticDomainError(ArithmeticError):
    """Raised when a candidate curve leaves the valid monotone domain."""


@dataclass(frozen=True)
class RootResult:
    value: float
    iterations: int
    inner_iterations: int = 0


def _bracketed_newton(
    function,
    derivative,
    low: float,
    high: float,
    *,
    tolerance: float = 2e-14,
) -> RootResult:
    """Solve one sign-changing bracket without assuming a Newton step is safe."""

    f_low = function(low)
    f_high = function(high)
    if not math.isfinite(f_low) or not math.isfinite(f_high):
        raise ArithmeticDomainError("non-finite root bracket")
    if f_low == 0.0:
        return RootResult(low, 0)
    if f_high == 0.0:
        return RootResult(high, 0)
    if f_low > 0.0 or f_high < 0.0:
        raise ArithmeticDomainError(
            f"expected increasing sign bracket, got {f_low=} {f_high=}"
        )

    value = (low + high) / 2.0
    for iteration in range(1, MAX_ROOT_ITERATIONS + 1):
        residual = function(value)
        if not math.isfinite(residual):
            raise ArithmeticDomainError("non-finite root residual")
        if residual <= 0.0:
            low = value
            f_low = residual
        else:
            high = value
            f_high = residual
        if high - low <= tolerance * max(1.0, abs(value)):
            return RootResult((low + high) / 2.0, iteration)

        slope = derivative(value)
        candidate = value - residual / slope if math.isfinite(slope) and slope > 0.0 else math.nan
        guard = (high - low) * 0.1
        if (
            not math.isfinite(candidate)
            or candidate <= low + guard
            or candidate >= high - guard
        ):
            candidate = (low + high) / 2.0
        value = candidate

    raise ArithmeticDomainError("root solver did not converge")


@dataclass(frozen=True)
class BridgeGeometry:
    q_start: float
    q_tail: float
    v_start: float
    v_tail: float
    q_prime_start: float
    length: float


@dataclass(frozen=True)
class Quote:
    amount_in: float
    amount_out: float
    reserve_in_after: float
    reserve_out_after: float
    q_root_iterations: int
    reserve_root_iterations: int
    used_exact_tail: bool


class HomogeneousAmm:
    """Symmetric two-asset homogeneous AMM described by ``q(v)``."""

    name: str = "AMM"

    def q_and_prime(self, v: float) -> tuple[float, float, int]:
        raise NotImplementedError

    def region_for_v(self, v: float) -> str:
        return "global"

    def q(self, v: float) -> float:
        return self.q_and_prime(v)[0]

    @staticmethod
    def imbalance(x: float, y: float) -> float:
        if x <= 0.0 or y <= 0.0:
            raise ValueError("reserves must be positive")
        return abs(x - y) / (2.0 * math.sqrt(x * y))

    def invariant(self, x: float, y: float) -> float:
        v = self.imbalance(x, y)
        q, _, _ = self.q_and_prime(v)
        if not 0.0 < q <= 1.0 + 1e-12:
            raise ArithmeticDomainError(f"invalid product ratio {q}")
        return 2.0 * math.sqrt(x * y / q)

    def coordinates(self, invariant: float, signed_u: float) -> tuple[float, float]:
        if invariant <= 0.0:
            raise ValueError("invariant must be positive")
        v = abs(math.sinh(signed_u))
        q, _, _ = self.q_and_prime(v)
        geometric = invariant * math.sqrt(q) / 2.0
        return geometric * math.exp(signed_u), geometric * math.exp(-signed_u)

    def marginal_price(self, x: float, y: float) -> float:
        """Return marginal units of y paid for one additional unit of x."""

        if x <= 0.0 or y <= 0.0:
            raise ValueError("reserves must be positive")
        if math.isclose(x, y, rel_tol=0.0, abs_tol=1e-15 * max(x, y)):
            return 1.0
        signed_u = 0.5 * math.log(x / y)
        v = abs(math.sinh(signed_u))
        q, q_prime, _ = self.q_and_prime(v)
        signed_dv_du = math.copysign(math.sqrt(1.0 + v * v), signed_u)
        radial_slope = q_prime * signed_dv_du / (2.0 * q)
        denominator = 1.0 + radial_slope
        if denominator <= 0.0:
            raise ArithmeticDomainError("non-monotone reserve coordinate")
        price = (y / x) * (1.0 - radial_slope) / denominator
        if not math.isfinite(price) or price <= 0.0:
            raise ArithmeticDomainError("non-positive marginal price")
        return price

    def _x_coordinate(self, invariant: float, v: float, high_branch: bool) -> tuple[float, float, int]:
        q, q_prime, q_iterations = self.q_and_prime(v)
        root = math.sqrt(1.0 + v * v)
        exponential = root + v if high_branch else root - v
        coordinate = invariant * math.sqrt(q) * exponential / 2.0
        log_derivative = q_prime / (2.0 * q) + (1.0 / root if high_branch else -1.0 / root)
        return coordinate, coordinate * log_derivative, q_iterations

    def _v_for_coordinate(self, invariant: float, target_x: float, high_branch: bool) -> RootResult:
        center = invariant / 2.0
        if math.isclose(target_x, center, rel_tol=2e-14, abs_tol=2e-14 * invariant):
            return RootResult(0.0, 0)
        if high_branch and target_x < center:
            raise ArithmeticDomainError("high branch target is below center")
        if not high_branch and target_x > center:
            raise ArithmeticDomainError("low branch target is above center")

        inner_iterations = 0
        high = 1.0
        for _ in range(256):
            coordinate, _, q_iterations = self._x_coordinate(invariant, high, high_branch)
            inner_iterations += q_iterations
            bracketed = coordinate >= target_x if high_branch else coordinate <= target_x
            if bracketed:
                break
            high *= 2.0
        else:
            raise ArithmeticDomainError("reserve coordinate root not bracketed")

        low = 0.0
        value = min(high, max(low, self.imbalance(target_x, max(invariant - target_x, EPSILON))))
        if value <= low or value >= high or not math.isfinite(value):
            value = (low + high) / 2.0

        for iteration in range(1, MAX_ROOT_ITERATIONS + 1):
            coordinate, slope, q_iterations = self._x_coordinate(invariant, value, high_branch)
            inner_iterations += q_iterations
            residual = coordinate - target_x
            if high_branch:
                if residual <= 0.0:
                    low = value
                else:
                    high = value
            else:
                if residual >= 0.0:
                    low = value
                else:
                    high = value
            if high - low <= 2e-13 * max(1.0, value):
                return RootResult((low + high) / 2.0, iteration, inner_iterations)
            candidate = value - residual / slope if slope != 0.0 and math.isfinite(slope) else math.nan
            guard = (high - low) * 0.1
            if (
                not math.isfinite(candidate)
                or candidate <= low + guard
                or candidate >= high - guard
            ):
                candidate = (low + high) / 2.0
            value = candidate
        raise ArithmeticDomainError("reserve coordinate solver did not converge")

    def reserve_y(self, x: float, invariant: float) -> tuple[float, int, int]:
        if x <= 0.0 or invariant <= 0.0:
            raise ValueError("reserve and invariant must be positive")
        high_branch = x >= invariant / 2.0
        solved = self._v_for_coordinate(invariant, x, high_branch)
        q, _, q_iterations = self.q_and_prime(solved.value)
        y = invariant * invariant * q / (4.0 * x)
        if not math.isfinite(y) or y <= 0.0:
            raise ArithmeticDomainError("invalid solved reserve")
        return y, q_iterations + solved.inner_iterations, solved.iterations

    def quote_exact_in(self, x: float, y: float, amount_in: float) -> Quote:
        if min(x, y, amount_in) <= 0.0:
            raise ValueError("reserves and amount must be positive")
        current_v = self.imbalance(x, y)
        current_q, _, current_q_iterations = self.q_and_prime(current_v)
        invariant = 2.0 * math.sqrt(x * y / current_q)
        new_x = x + amount_in
        new_y, q_iterations, reserve_iterations = self.reserve_y(new_x, invariant)
        output = y - new_y
        if output <= 0.0:
            raise ArithmeticDomainError("non-positive swap output")
        return Quote(
            amount_in=amount_in,
            amount_out=output,
            reserve_in_after=new_x,
            reserve_out_after=new_y,
            q_root_iterations=current_q_iterations + q_iterations,
            reserve_root_iterations=reserve_iterations,
            used_exact_tail=False,
        )

    def quote_exact_out(self, x: float, y: float, amount_out: float) -> Quote:
        if min(x, y, amount_out) <= 0.0 or amount_out >= y:
            raise ValueError("invalid reserves or exact output")
        current_v = self.imbalance(x, y)
        current_q, _, current_q_iterations = self.q_and_prime(current_v)
        invariant = 2.0 * math.sqrt(x * y / current_q)
        new_y = y - amount_out
        new_x, q_iterations, reserve_iterations = self.reserve_y(new_y, invariant)
        amount_in = new_x - x
        if amount_in <= 0.0:
            raise ArithmeticDomainError("non-positive exact-output input")
        return Quote(
            amount_in=amount_in,
            amount_out=amount_out,
            reserve_in_after=new_x,
            reserve_out_after=new_y,
            q_root_iterations=current_q_iterations + q_iterations,
            reserve_root_iterations=reserve_iterations,
            used_exact_tail=False,
        )


@dataclass(frozen=True)
class Cpmm(HomogeneousAmm):
    name: ClassVar[str] = "CPMM"

    def q_and_prime(self, v: float) -> tuple[float, float, int]:
        if v < 0.0:
            raise ValueError("imbalance must be non-negative")
        return 1.0, 0.0, 0

    def reserve_y(self, x: float, invariant: float) -> tuple[float, int, int]:
        return invariant * invariant / (4.0 * x), 0, 0

    def quote_exact_in(self, x: float, y: float, amount_in: float) -> Quote:
        if min(x, y, amount_in) <= 0.0:
            raise ValueError("reserves and amount must be positive")
        new_x = x + amount_in
        new_y = x * y / new_x
        return Quote(amount_in, y - new_y, new_x, new_y, 0, 0, True)


@dataclass(frozen=True)
class CryptoSwap(HomogeneousAmm):
    """Published two-asset CryptoSwap invariant in Dusk-native parameters."""

    peak_depth: float = 200.0
    fade_scale: float = 0.001
    name: ClassVar[str] = "CryptoSwap"

    def __post_init__(self) -> None:
        if self.peak_depth <= 0.0 or self.fade_scale <= 0.0:
            raise ValueError("CryptoSwap parameters must be positive")

    def _inner_residual(self, q: float, v: float) -> float:
        delta = 1.0 - q
        weight = (self.fade_scale / (self.fade_scale + delta)) ** 2
        h = math.sqrt(q) * math.sqrt(1.0 + v * v) - 1.0
        return 2.0 * self.peak_depth * q * weight * h + q - 1.0

    def _inner_derivatives(self, q: float, v: float) -> tuple[float, float]:
        delta = 1.0 - q
        denominator = self.fade_scale + delta
        weight = (self.fade_scale / denominator) ** 2
        coefficient = 2.0 * self.peak_depth * q * weight
        cosh = math.sqrt(1.0 + v * v)
        h = math.sqrt(q) * cosh - 1.0
        coefficient_q = coefficient * (1.0 / q + 2.0 / denominator)
        residual_q = coefficient_q * h + coefficient * cosh / (2.0 * math.sqrt(q)) + 1.0
        residual_v = coefficient * math.sqrt(q) * v / cosh
        return residual_q, residual_v

    def q_and_prime(self, v: float) -> tuple[float, float, int]:
        if v < 0.0 or not math.isfinite(v):
            raise ValueError("imbalance must be finite and non-negative")
        if v <= 1e-16:
            return 1.0, 0.0, 0

        low = max(1e-300, min(0.5, 1.0 / (1.0 + v ** (2.0 / 3.0))))
        while self._inner_residual(low, v) >= 0.0:
            low *= 0.5
            if low <= 1e-300:
                break

        result = _bracketed_newton(
            lambda q: self._inner_residual(q, v),
            lambda q: self._inner_derivatives(q, v)[0],
            low,
            1.0,
        )
        residual_q, residual_v = self._inner_derivatives(result.value, v)
        q_prime = -residual_v / residual_q
        if q_prime > 1e-12:
            raise ArithmeticDomainError("CryptoSwap q increased with imbalance")
        return result.value, min(q_prime, 0.0), result.iterations


@dataclass(frozen=True)
class CryptoSwapLimit(HomogeneousAmm):
    """Singular CryptoSwap envelope as fade→0 and peak_depth→∞ at fixed P*s²."""

    concentration_budget: float
    name: ClassVar[str] = "CryptoSwap singular limit"

    def __post_init__(self) -> None:
        if self.concentration_budget <= 0.0:
            raise ValueError("concentration budget must be positive")

    def _residual(self, q: float, v: float) -> float:
        delta = 1.0 - q
        h = math.sqrt(q) * math.sqrt(1.0 + v * v) - 1.0
        return 2.0 * self.concentration_budget * q * h - delta**3

    def _derivatives(self, q: float, v: float) -> tuple[float, float]:
        delta = 1.0 - q
        cosh = math.sqrt(1.0 + v * v)
        h = math.sqrt(q) * cosh - 1.0
        residual_q = (
            2.0
            * self.concentration_budget
            * (h + math.sqrt(q) * cosh / 2.0)
            + 3.0 * delta * delta
        )
        residual_v = (
            2.0
            * self.concentration_budget
            * q
            * math.sqrt(q)
            * v
            / cosh
        )
        return residual_q, residual_v

    def q_and_prime(self, v: float) -> tuple[float, float, int]:
        if v < 0.0 or not math.isfinite(v):
            raise ValueError("imbalance must be finite and non-negative")
        if v <= 1e-16:
            return 1.0, 0.0, 0
        result = _bracketed_newton(
            lambda q: self._residual(q, v),
            lambda q: self._derivatives(q, v)[0],
            1e-300,
            1.0,
        )
        residual_q, residual_v = self._derivatives(result.value, v)
        q_prime = -residual_v / residual_q
        if q_prime > 1e-12:
            raise ArithmeticDomainError("CryptoSwap limit q increased with imbalance")
        return result.value, min(q_prime, 0.0), result.iterations


@dataclass(frozen=True)
class DuskC1(CryptoSwap):
    """CryptoSwap center plus a fixed C1 convergence to exact CPMM tails."""

    bridge_start_fraction: float = 0.5
    name: ClassVar[str] = "Dusk C1"

    def __post_init__(self) -> None:
        super().__post_init__()
        if not 0.0 < self.bridge_start_fraction < 1.0:
            raise ValueError("bridge start fraction must be inside (0, 1)")
        geometry = self.bridge_geometry()
        if geometry.length <= 0.0 or geometry.v_tail <= geometry.v_start:
            raise ArithmeticDomainError("invalid C1 bridge geometry")

    def bridge_geometry(self) -> BridgeGeometry:
        q_start = 1.0 - self.bridge_start_fraction * self.fade_scale
        q_tail = 1.0 - self.fade_scale
        if q_tail <= 0.0:
            raise ArithmeticDomainError("fade scale leaves no CPMM product")
        delta = 1.0 - q_start
        weight = (self.fade_scale / (self.fade_scale + delta)) ** 2
        h = delta / (2.0 * self.peak_depth * q_start * weight)
        cosh = (1.0 + h) / math.sqrt(q_start)
        v_start = math.sqrt(max(cosh * cosh - 1.0, 0.0))
        residual_q, residual_v = self._inner_derivatives(q_start, v_start)
        q_prime_start = -residual_v / residual_q
        if not q_prime_start < 0.0:
            raise ArithmeticDomainError("bridge starts without an outward fade")
        length = 3.0 * (q_tail - q_start) / q_prime_start
        return BridgeGeometry(
            q_start=q_start,
            q_tail=q_tail,
            v_start=v_start,
            v_tail=v_start + length,
            q_prime_start=q_prime_start,
            length=length,
        )

    def region_for_v(self, v: float) -> Literal["inner", "transition", "tail"]:
        geometry = self.bridge_geometry()
        if v < geometry.v_start:
            return "inner"
        if v < geometry.v_tail:
            return "transition"
        return "tail"

    def q_and_prime(self, v: float) -> tuple[float, float, int]:
        if v < 0.0 or not math.isfinite(v):
            raise ValueError("imbalance must be finite and non-negative")
        geometry = self.bridge_geometry()
        if v < geometry.v_start:
            return super().q_and_prime(v)
        if v >= geometry.v_tail:
            return geometry.q_tail, 0.0, 0
        z = (v - geometry.v_start) / geometry.length
        q = geometry.q_start + geometry.q_prime_start * geometry.length * (
            z - z * z + z * z * z / 3.0
        )
        q_prime = geometry.q_prime_start * (1.0 - z) ** 2
        return q, q_prime, 0

    def quote_exact_in(self, x: float, y: float, amount_in: float) -> Quote:
        if min(x, y, amount_in) <= 0.0:
            raise ValueError("reserves and amount must be positive")
        current_v = self.imbalance(x, y)
        current_q, _, current_q_iterations = self.q_and_prime(current_v)
        invariant = 2.0 * math.sqrt(x * y / current_q)
        new_x = x + amount_in
        # Any move that stays on the same exact-tail side preserves one xy=k
        # level and requires no nonlinear reserve solve.  A move that crosses
        # the transition or center must use the full curve.
        if self.region_for_v(current_v) == "tail":
            new_y = x * y / new_x
            new_v = self.imbalance(new_x, new_y)
            same_side = (x - y) * (new_x - new_y) > 0.0
            if same_side and self.region_for_v(new_v) == "tail":
                return Quote(amount_in, y - new_y, new_x, new_y, 0, 0, True)
        new_y, q_iterations, reserve_iterations = self.reserve_y(new_x, invariant)
        output = y - new_y
        if output <= 0.0:
            raise ArithmeticDomainError("non-positive swap output")
        return Quote(
            amount_in,
            output,
            new_x,
            new_y,
            current_q_iterations + q_iterations,
            reserve_iterations,
            False,
        )

    def quote_exact_out(self, x: float, y: float, amount_out: float) -> Quote:
        if min(x, y, amount_out) <= 0.0 or amount_out >= y:
            raise ValueError("invalid reserves or exact output")
        current_v = self.imbalance(x, y)
        if self.region_for_v(current_v) == "tail":
            new_y = y - amount_out
            new_x = x * y / new_y
            new_v = self.imbalance(new_x, new_y)
            same_side = (x - y) * (new_x - new_y) > 0.0
            if same_side and self.region_for_v(new_v) == "tail":
                return Quote(new_x - x, amount_out, new_x, new_y, 0, 0, True)
        return super().quote_exact_out(x, y, amount_out)


def normalized_reserves_at_price(model: HomogeneousAmm, target_price: float) -> tuple[float, float, int]:
    """Return the D=2 reserve state whose marginal price equals target_price."""

    if target_price <= 0.0 or not math.isfinite(target_price):
        raise ValueError("target price must be finite and positive")
    if math.isclose(target_price, 1.0, rel_tol=0.0, abs_tol=1e-14):
        return 1.0, 1.0, 0
    reciprocal = target_price > 1.0
    directed_target = 1.0 / target_price if reciprocal else target_price

    low = 0.0
    high = 1.0
    for _ in range(256):
        x, y = model.coordinates(2.0, math.asinh(high))
        if model.marginal_price(x, y) <= directed_target:
            break
        high *= 2.0
    else:
        raise ArithmeticDomainError("price state not bracketed")

    iterations = 0
    for iterations in range(1, MAX_ROOT_ITERATIONS + 1):
        middle = (low + high) / 2.0
        x, y = model.coordinates(2.0, math.asinh(middle))
        price = model.marginal_price(x, y)
        if price > directed_target:
            low = middle
        else:
            high = middle
        if high - low <= 2e-13 * max(1.0, middle):
            break
    else:
        raise ArithmeticDomainError("price-state solver did not converge")
    v = (low + high) / 2.0
    x, y = model.coordinates(2.0, math.asinh(v))
    return (y, x, iterations) if reciprocal else (x, y, iterations)


def execution_price_premium_bps(model: HomogeneousAmm, trade_size_bps_of_tvl: float) -> float:
    """Average input/output price premium over the balanced marginal price."""

    amount_in = 2.0 * trade_size_bps_of_tvl / 10_000.0
    quote = model.quote_exact_in(1.0, 1.0, amount_in)
    return (amount_in / quote.amount_out - 1.0) * 10_000.0


def output_shortfall_bps(model: HomogeneousAmm, trade_size_bps_of_tvl: float) -> float:
    """Exact-input output shortfall relative to the balanced marginal quote."""

    amount_in = 2.0 * trade_size_bps_of_tvl / 10_000.0
    quote = model.quote_exact_in(1.0, 1.0, amount_in)
    return (1.0 - quote.amount_out / amount_in) * 10_000.0


def jump_loss_bps(model: HomogeneousAmm, external_price: float) -> tuple[float, float, float]:
    """Fee-free one-jump arbitrage loss versus holding the initial inventory.

    The normalization is basis points of the initial value (2 at the starting
    price), not basis points of the larger post-jump hold value.  For a single
    jump this is also the arbitrageur's mark-to-market profit.  It is not a
    multi-step path-LVR or an expected LP-return model.
    """

    x, y, _ = normalized_reserves_at_price(model, external_price)
    wealth = x * external_price + y
    hold = external_price + 1.0
    loss = hold - wealth
    if loss < -2e-10 * max(hold, 1.0):
        raise ArithmeticDomainError("post-arbitrage pool exceeded hold value")
    loss = max(loss, 0.0)
    return loss / 2.0 * 10_000.0, x, y
