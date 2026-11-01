"""Independent analytical AMM models used by the decision benchmark."""

from __future__ import annotations

from dataclasses import dataclass, replace
from decimal import Decimal, localcontext
import math
from typing import Callable, ClassVar, Optional


EPS = 1e-12


def bisect_root(function: Callable[[float], float], low: float, high: float, iterations: int = 100) -> float:
    f_low = function(low)
    f_high = function(high)
    if not math.isfinite(f_low) or not math.isfinite(f_high):
        raise ArithmeticError("non-finite root bracket")
    if f_low == 0:
        return low
    if f_high == 0:
        return high
    if f_low * f_high > 0:
        raise ArithmeticError(f"root not bracketed: {f_low=} {f_high=}")
    for _ in range(iterations):
        middle = (low + high) / 2
        # Once the midpoint rounds to an endpoint, further float bisections
        # repeat the exact same residual and cannot refine the bracket.  The
        # old fixed-count loop returned this same rounded midpoint after the
        # remaining iterations, so stopping here is bit-for-bit equivalent.
        if middle == low or middle == high:
            break
        f_middle = function(middle)
        if f_middle == 0:
            return middle
        if f_low * f_middle <= 0:
            high = middle
            f_high = f_middle
        else:
            low = middle
            f_low = f_middle
    return (low + high) / 2


def scan_bracketed_roots(
    function: Callable[[float], float],
    low: float,
    high: float,
    *,
    intervals: int = 32_768,
) -> list[float]:
    """Find every sign-changing root on a deterministic linear grid.

    This is a safety scanner, not a production solver.  It intentionally does
    not assume uniqueness, which is exactly the assumption under test for
    experimental invariant profiles.
    """
    if intervals <= 0 or not 0 < low <= high:
        raise ValueError("invalid root-scan domain")
    roots: list[float] = []
    previous_x = low
    previous_value = function(low)
    if previous_value == 0.0:
        roots.append(low)
    for index in range(1, intervals + 1):
        current_x = low + (high - low) * index / intervals
        current_value = function(current_x)
        if not math.isfinite(current_value):
            raise ArithmeticError("non-finite value during root scan")
        if current_value == 0.0:
            root = current_x
        elif previous_value * current_value < 0.0:
            root = bisect_root(function, previous_x, current_x, 100)
        else:
            previous_x = current_x
            previous_value = current_value
            continue
        if not roots or abs(root - roots[-1]) > max(1e-11, abs(root) * 1e-10):
            roots.append(root)
        previous_x = current_x
        previous_value = current_value
    return roots


@dataclass(frozen=True)
class Quote:
    amount_in: float
    amount_out: float
    fee_paid: float
    direction: str
    marginal_before: float
    marginal_after: float


class Amm:
    name = "amm"

    def invariant(self, x: float, y: float) -> float:
        raise NotImplementedError

    def reserve_y(self, x: float, invariant: float, y_hint: float) -> float:
        low = max(y_hint * 1e-9, EPS)
        high = max(y_hint * 1e3, invariant * 1e3, 1.0)
        function = lambda y: self.invariant(x, y) - invariant
        while function(low) * function(high) > 0:
            low *= 0.1
            high *= 10
            if high > 1e30:
                raise ArithmeticError("reserve root not bracketed")
        return bisect_root(function, low, high)

    def fee_rate(self, x: float, y: float, volatility: float = 0.0) -> float:
        return 0.0

    def marginal_price(self, x: float, y: float) -> float:
        probe = max(x * 1e-7, 1e-7)
        quote = self.quote(x, y, probe, "x_to_y", apply_fee=False)
        return quote.amount_out / probe

    def quote(
        self,
        x: float,
        y: float,
        amount_in: float,
        direction: str,
        *,
        apply_fee: bool = True,
        volatility: float = 0.0,
        include_marginals: bool = True,
        fee_rate_override: Optional[float] = None,
        invariant_override: Optional[float] = None,
        marginal_before_override: Optional[float] = None,
    ) -> Quote:
        if min(x, y, amount_in) <= 0:
            raise ValueError("reserves and input must be positive")
        marginal_before = (
            marginal_before_override
            if include_marginals and marginal_before_override is not None
            else self.marginal_price_raw(x, y)
            if include_marginals
            else math.nan
        )
        fee_rate = (
            fee_rate_override
            if apply_fee and fee_rate_override is not None
            else self.fee_rate(x, y, volatility) if apply_fee else 0.0
        )
        effective_in = amount_in * (1.0 - fee_rate)
        invariant = (
            invariant_override
            if invariant_override is not None
            else self.invariant(x, y)
        )
        if direction == "x_to_y":
            new_x = x + effective_in
            new_y = self.reserve_y(new_x, invariant, y)
            amount_out = y - new_y
            marginal_after = self.marginal_price_raw(new_x, new_y) if include_marginals else math.nan
        elif direction == "y_to_x":
            # Symmetry lets the same solver operate in reversed coordinates.
            new_y = y + effective_in
            new_x = self.reserve_y(new_y, invariant, x)
            amount_out = x - new_x
            marginal_after = 1.0 / self.marginal_price_raw(new_y, new_x) if include_marginals else math.nan
        else:
            raise ValueError(f"unknown direction {direction}")
        if amount_out <= 0 or not math.isfinite(amount_out):
            raise ArithmeticError("invalid quote output")
        return Quote(
            amount_in=amount_in,
            amount_out=amount_out,
            fee_paid=amount_in * fee_rate,
            direction=direction,
            marginal_before=marginal_before,
            marginal_after=marginal_after,
        )

    def marginal_price_raw(self, x: float, y: float) -> float:
        probe = max(x * 1e-7, 1e-9)
        invariant = self.invariant(x, y)
        new_y = self.reserve_y(x + probe, invariant, y)
        return (y - new_y) / probe


@dataclass(frozen=True)
class Cpmm(Amm):
    name: str = "CPMM"

    def invariant(self, x: float, y: float) -> float:
        return math.sqrt(x * y)

    def reserve_y(self, x: float, invariant: float, y_hint: float) -> float:
        return invariant * invariant / x

    def marginal_price_raw(self, x: float, y: float) -> float:
        return y / x


@dataclass(frozen=True)
class CurrentDh2(Amm):
    """Production DH2 equation under its observable two-knob interface.

    ``peak_depth`` is the additional center marginal-depth multiplier.  The
    legacy equation's internal amplification coefficient is therefore
    ``peak_depth / 2``.  ``fade_scale`` controls how quickly that extra depth
    decays with squared normalized reserve imbalance.  Setting both to zero
    is exact CPMM.
    """

    peak_depth: float = 200.0
    fade_scale: float = 0.1
    base_fee: float = 0.0005
    divergence_coefficient: float = 0.02
    volatility_coefficient: float = 0.02
    max_dynamic_fee: float = 0.02
    name: str = "Current DH2"

    def __post_init__(self) -> None:
        cpmm = self.peak_depth == 0.0 and self.fade_scale == 0.0
        concentrated = self.peak_depth > 0.0 and self.fade_scale > 0.0
        if not (cpmm or concentrated):
            raise ValueError(
                "peak_depth and fade_scale must either both be zero or both be positive"
            )

    @property
    def legacy_amplification(self) -> float:
        return self.peak_depth / 2.0

    def _effective_depth(self, x: float, y: float, d: float) -> float:
        if self.peak_depth == 0.0:
            return 0.0
        imbalance = ((x - y) / d) ** 2
        return (
            self.legacy_amplification
            * self.fade_scale
            / (self.fade_scale + imbalance)
        )

    def _residual(self, d: float, x: float, y: float) -> float:
        # This is the hot path for every invariant and reserve bisection.
        # Keep the arithmetic order identical to ``_effective_depth`` while
        # avoiding millions of method/property dispatches in broad sweeps.
        if type(self) is CurrentDh2:
            if self.peak_depth == 0.0:
                effective_depth = 0.0
            else:
                imbalance = ((x - y) / d) ** 2
                effective_depth = (
                    (self.peak_depth / 2.0)
                    * self.fade_scale
                    / (self.fade_scale + imbalance)
                )
        else:
            effective_depth = self._effective_depth(x, y, d)
        # Algebraically divide the original residual by D. Grouping the
        # cancellation terms keeps the reference stable near balance.
        return (
            4 * effective_depth * ((x + y) / d - 1)
            + 1
            - d * d / (4 * x * y)
        )

    def _decimal_residual(self, d: Decimal, x: Decimal, y: Decimal) -> Decimal:
        four = Decimal(4)
        if self.peak_depth == 0.0:
            return d - d**3 / (four * x * y)
        fade_scale = Decimal(str(self.fade_scale))
        amplification = Decimal(str(self.legacy_amplification))
        imbalance = ((x - y) / d) ** 2
        effective_depth = amplification * fade_scale / (fade_scale + imbalance)
        return (
            four * effective_depth * (x + y)
            + d
            - four * effective_depth * d
            - d**3 / (four * x * y)
        )

    def invariant(self, x: float, y: float) -> float:
        low = 2 * math.sqrt(x * y)
        high = x + y
        if math.isclose(low, high, rel_tol=1e-15):
            return high
        # At very small fade scales the concentrated term can underflow to
        # zero and leave a one-ulp negative CPMM endpoint residual. Treat only
        # that numerical endpoint case as the exact lower root.
        low_residual = self._residual(low, x, y)
        if abs(low_residual) <= 1e-14:
            return low
        return bisect_root(lambda d: self._residual(d, x, y), low, high, 80)

    def reserve_y(self, x: float, invariant: float, y_hint: float) -> float:
        low = max(y_hint * 1e-9, EPS)
        high = max(y_hint * 1e3, invariant * 1e3, 1.0)
        function = lambda y: self._residual(invariant, x, y)
        return bisect_root(function, low, high, 80)

    def marginal_price_raw(self, x: float, y: float) -> float:
        d = self.invariant(x, y)
        return self.marginal_price_raw_with_invariant(x, y, d)

    def marginal_price_raw_with_invariant(
        self,
        x: float,
        y: float,
        invariant: float,
    ) -> float:
        """Evaluate the existing finite-difference marginal for a known D."""

        d = invariant
        x_step = max(x * 1e-6, 1e-9)
        y_step = max(y * 1e-6, 1e-9)
        derivative_x = (
            self._residual(d, x + x_step, y) - self._residual(d, x - x_step, y)
        ) / (2 * x_step)
        derivative_y = (
            self._residual(d, x, y + y_step) - self._residual(d, x, y - y_step)
        ) / (2 * y_step)
        return derivative_x / derivative_y

    def fee_rate(self, x: float, y: float, volatility: float = 0.0) -> float:
        divergence = abs(math.log(x / y))
        surcharge = self.divergence_coefficient * divergence * divergence
        surcharge += self.volatility_coefficient * volatility * volatility
        return min(self.base_fee + surcharge, self.base_fee + self.max_dynamic_fee)


@dataclass(frozen=True)
class SharpDh2(CurrentDh2):
    """Rejected analytical sharper-fade experiment.

    The exponent is protocol-fixed at two; it is deliberately not an operator
    parameter.  This profile decays more slowly than production DH2 inside the
    useful shoulder and faster after ``u / fade_scale = 1``.  It is retained
    only to reproduce the safety rejection: because its effective depth
    depends on the solved invariant ``D`` more strongly, some reserve states
    have multiple valid invariant roots.

    This class is a high-precision analytical experiment, not Rust validated
    and not a production candidate.
    """

    SHAPE_EXPONENT: ClassVar[int] = 2
    name: str = "Sharp DH2 (rejected)"

    def _effective_depth(self, x: float, y: float, d: float) -> float:
        if self.peak_depth == 0.0:
            return 0.0
        imbalance = ((x - y) / d) ** 2
        normalized = imbalance / self.fade_scale
        return self.legacy_amplification / (
            1.0 + normalized**self.SHAPE_EXPONENT
        )

    def _decimal_residual(self, d: Decimal, x: Decimal, y: Decimal) -> Decimal:
        four = Decimal(4)
        if self.peak_depth == 0.0:
            return d - d**3 / (four * x * y)
        fade_scale = Decimal(str(self.fade_scale))
        amplification = Decimal(str(self.legacy_amplification))
        imbalance = ((x - y) / d) ** 2
        normalized = imbalance / fade_scale
        effective_depth = amplification / (
            Decimal(1) + normalized**self.SHAPE_EXPONENT
        )
        return (
            four * effective_depth * (x + y)
            + d
            - four * effective_depth * d
            - d**3 / (four * x * y)
        )


@dataclass(frozen=True)
class HyperbolicSharpDh2(CurrentDh2):
    """D-independent sharp-tail experiment retained for economic rejection.

    Unlike :class:`SharpDh2`, its fade coordinate does not contain the solved
    invariant D, avoiding the demonstrated multi-root feedback.  The fourth
    power is protocol-fixed.  It is still analytical-only and is not part of
    the production family.
    """

    SHAPE_EXPONENT: ClassVar[int] = 4
    name: str = "Hyperbolic p4 DH2 (rejected)"

    def _effective_depth(self, x: float, y: float, d: float) -> float:
        del d
        if self.peak_depth == 0.0:
            return 0.0
        hyperbolic_distance = abs(x - y) / (2.0 * math.sqrt(x * y))
        normalized = hyperbolic_distance / self.fade_scale
        return self.legacy_amplification / (
            1.0 + normalized**self.SHAPE_EXPONENT
        )

    def _decimal_residual(self, d: Decimal, x: Decimal, y: Decimal) -> Decimal:
        four = Decimal(4)
        if self.peak_depth == 0.0:
            return d - d**3 / (four * x * y)
        fade_scale = Decimal(str(self.fade_scale))
        amplification = Decimal(str(self.legacy_amplification))
        hyperbolic_distance = abs(x - y) / (Decimal(2) * (x * y).sqrt())
        normalized = hyperbolic_distance / fade_scale
        effective_depth = amplification / (
            Decimal(1) + normalized**self.SHAPE_EXPONENT
        )
        return (
            four * effective_depth * (x + y)
            + d
            - four * effective_depth * d
            - d**3 / (four * x * y)
        )


@dataclass(frozen=True)
class CryptoSwap(Amm):
    """Two-asset CryptoSwap equation reconstructed from the whitepaper."""

    amplification: float = 400.0
    gamma: float = 0.001
    mid_fee: float = 0.0005
    out_fee: float = 0.0045
    fee_gamma: float = 0.01
    name: str = "Curve CryptoSwap"

    def _k(self, d: float, x: float, y: float) -> float:
        k0 = 4 * x * y / (d * d)
        denominator = self.gamma + 1.0 - k0
        return self.amplification * k0 * self.gamma * self.gamma / (denominator * denominator)

    def _residual(self, d: float, x: float, y: float) -> float:
        k = self._k(d, x, y)
        # Divide by D² and group the K terms to avoid catastrophic
        # cancellation around the balanced point.
        return k * ((x + y) / d - 1) + x * y / (d * d) - 0.25

    def _decimal_residual(self, d: Decimal, x: Decimal, y: Decimal) -> Decimal:
        four = Decimal(4)
        k0 = four * x * y / (d * d)
        gamma = Decimal(str(self.gamma))
        amplification = Decimal(str(self.amplification))
        denominator = gamma + Decimal(1) - k0
        k = amplification * k0 * gamma * gamma / (denominator * denominator)
        return k * d * (x + y) + x * y - k * d * d - d * d / four

    def invariant(self, x: float, y: float) -> float:
        low = 2 * math.sqrt(x * y)
        high = x + y
        if math.isclose(low, high, rel_tol=1e-15):
            return high
        return bisect_root(lambda d: self._residual(d, x, y), low, high, 90)

    def reserve_y(self, x: float, invariant: float, y_hint: float) -> float:
        low = max(y_hint * 1e-9, EPS)
        high = max(y_hint * 1e3, invariant * 1e3, 1.0)
        return bisect_root(lambda y: self._residual(invariant, x, y), low, high, 90)

    def marginal_price_raw(self, x: float, y: float) -> float:
        d = self.invariant(x, y)
        x_step = max(x * 1e-6, 1e-9)
        y_step = max(y * 1e-6, 1e-9)
        derivative_x = (
            self._residual(d, x + x_step, y) - self._residual(d, x - x_step, y)
        ) / (2 * x_step)
        derivative_y = (
            self._residual(d, x, y + y_step) - self._residual(d, x, y - y_step)
        ) / (2 * y_step)
        return derivative_x / derivative_y

    def fee_rate(self, x: float, y: float, volatility: float = 0.0) -> float:
        balance = 4 * x * y / ((x + y) ** 2)
        weight = self.fee_gamma / (self.fee_gamma + 1.0 - balance)
        return self.out_fee - weight * (self.out_fee - self.mid_fee)


@dataclass(frozen=True)
class LogPotentialDh2(Amm):
    depth_multiplier: float = 20.0
    half_width_bps: float = 2_000.0
    base_fee: float = 0.0005
    divergence_coefficient: float = 0.02
    volatility_coefficient: float = 0.02
    max_dynamic_fee: float = 0.02
    name: str = "Proposed DH2"

    @property
    def width(self) -> float:
        return 0.5 * math.log1p(self.half_width_bps / 10_000.0)

    @property
    def curvature(self) -> float:
        return 1.0 - 1.0 / self.depth_multiplier

    def potential(self, z: float) -> float:
        width = self.width
        if width <= 0 or self.depth_multiplier <= 1:
            return 0.0
        t = abs(z) / width
        cap = self.curvature * width * width / 8.0
        if t >= 1:
            return cap
        return cap * (1.0 - (1.0 - t * t) ** 4)

    def invariant(self, x: float, y: float) -> float:
        z = 0.5 * math.log(x / y)
        return math.sqrt(x * y) * math.exp(self.potential(z))

    def fee_rate(self, x: float, y: float, volatility: float = 0.0) -> float:
        divergence = abs(math.log(x / y))
        surcharge = self.divergence_coefficient * divergence * divergence
        surcharge += self.volatility_coefficient * volatility * volatility
        return min(self.base_fee + surcharge, self.base_fee + self.max_dynamic_fee)

    def marginal_price_raw(self, x: float, y: float) -> float:
        z = 0.5 * math.log(x / y)
        width = self.width
        derivative = 0.0
        if width > 0 and abs(z) < width and self.depth_multiplier > 1:
            t2 = (z / width) ** 2
            derivative = self.curvature * z * (1.0 - t2) ** 3
        if abs(derivative) >= 1:
            raise ArithmeticError("non-positive marginal price")
        return (y / x) * (1.0 + derivative) / (1.0 - derivative)


def with_zero_fees(model: Amm) -> Amm:
    if isinstance(model, CryptoSwap):
        return replace(model, mid_fee=0.0, out_fee=0.0)
    if isinstance(model, (CurrentDh2, LogPotentialDh2)):
        return replace(
            model,
            base_fee=0.0,
            divergence_coefficient=0.0,
            volatility_coefficient=0.0,
            max_dynamic_fee=0.0,
        )
    return model
