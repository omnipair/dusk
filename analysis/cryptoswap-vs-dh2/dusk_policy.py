"""Integer-exact reference for Dusk's swap fee and retention policy.

This module mirrors the fee ordering in:

* ``programs/dusk/src/math/dynamic_fee.rs``
* ``programs/dusk/src/state/market/swap_engine.rs``

It deliberately does not contain an AMM invariant.  A simulator first calls
``assess_swap_fee`` with invariant input-reserve coordinates, quotes its AMM
with ``amount_in_for_quote``, and then calls ``volatility_after_assessed_swap``
with the actual marginal prices produced by that quote.

All rates use NAD precision (``1_000_000_000 == 100%``).  All token amounts and
reserve coordinates must be non-negative integers in one consistent unit.
"""

from __future__ import annotations

from dataclasses import dataclass, replace


NAD = 1_000_000_000
TARGET_MS_PER_SLOT = 400
NATURAL_LOG_OF_TWO_NAD = 693_147_180
TAYLOR_TERMS = 5
U64_MAX = (1 << 64) - 1


def _require_u64(value: int, name: str) -> None:
    if not isinstance(value, int) or isinstance(value, bool) or not 0 <= value <= U64_MAX:
        raise ValueError(f"{name} must be a u64")


def _require_nonnegative_int(value: int, name: str) -> None:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise ValueError(f"{name} must be a non-negative integer")


def ceil_div(numerator: int, denominator: int) -> int:
    _require_nonnegative_int(numerator, "numerator")
    if denominator <= 0:
        raise ValueError("denominator must be positive")
    if numerator == 0:
        return 0
    return (numerator - 1) // denominator + 1


def taylor_exp_nad(x_nad: int) -> int:
    """Port of Dusk's fixed-point ``taylor_exp(x, NAD, 5)``."""

    if not isinstance(x_nad, int) or isinstance(x_nad, bool):
        raise ValueError("x_nad must be an integer")
    is_negative = x_nad < 0
    absolute = -x_nad if is_negative else x_nad
    reduced = absolute // 10

    term = NAD
    total = NAD
    for index in range(1, TAYLOR_TERMS + 1):
        term = term * reduced // (index * NAD)
        total += term

    result = NAD
    for _ in range(10):
        result = result * total // NAD
    if is_negative:
        result = NAD * NAD // result
    return result


def exponential_decay_nad(start_nad: int, elapsed_ms: int, half_life_ms: int) -> int:
    """Decay an integer signal with the same bounded approximation as Dusk."""

    _require_u64(start_nad, "start_nad")
    _require_u64(elapsed_ms, "elapsed_ms")
    _require_u64(half_life_ms, "half_life_ms")
    if half_life_ms == 0 or start_nad == 0:
        return 0
    exponent_nad = elapsed_ms * NATURAL_LOG_OF_TWO_NAD // half_life_ms
    # Rust caps before converting to i64.
    exponent_nad = min(exponent_nad, (1 << 63) - 1)
    alpha_nad = taylor_exp_nad(-exponent_nad)
    result = start_nad * alpha_nad // NAD
    _require_u64(result, "decayed result")
    return result


@dataclass(frozen=True)
class DynamicFeeConfig:
    base_fee_rate_nad: int
    max_fee_rate_nad: int
    divergence_coefficient_nad: int
    volatility_coefficient_nad: int
    volatility_half_life_ms: int
    volatility_shock_cap_nad: int
    volatility_accumulator_cap_nad: int

    def validate(self) -> None:
        for field_name, value in self.__dict__.items():
            _require_u64(value, field_name)
        if self.base_fee_rate_nad > self.max_fee_rate_nad or self.max_fee_rate_nad > NAD:
            raise ValueError("fee rates must satisfy base <= maximum <= NAD")
        if self.volatility_shock_cap_nad > self.volatility_accumulator_cap_nad:
            raise ValueError("volatility shock cap cannot exceed accumulator cap")
        if self.volatility_coefficient_nad and self.volatility_half_life_ms == 0:
            raise ValueError("enabled volatility fees require a positive half-life")


@dataclass(frozen=True)
class DynamicFeePreState:
    center_price_nad: int
    volatility_accumulator_nad: int
    volatility_last_update_slot: int

    def validate(self) -> None:
        _require_u64(self.center_price_nad, "center_price_nad")
        _require_u64(self.volatility_accumulator_nad, "volatility_accumulator_nad")
        _require_u64(self.volatility_last_update_slot, "volatility_last_update_slot")


@dataclass(frozen=True)
class DynamicFeePath:
    amount_in: int
    start_price_nad: int
    end_price_nad: int
    current_slot: int
    divergence_surcharge_amount: int

    def validate(self) -> None:
        _require_u64(self.amount_in, "amount_in")
        _require_u64(self.start_price_nad, "start_price_nad")
        _require_u64(self.end_price_nad, "end_price_nad")
        _require_u64(self.current_slot, "current_slot")
        _require_u64(self.divergence_surcharge_amount, "divergence_surcharge_amount")
        if self.amount_in == 0:
            raise ValueError("amount_in must be positive")


@dataclass(frozen=True)
class DynamicFeeQuote:
    base_rate_nad: int
    divergence_rate_nad: int
    volatility_rate_nad: int
    total_rate_nad: int
    base_fee_amount: int
    divergence_surcharge_amount: int
    volatility_surcharge_amount: int
    dynamic_surcharge_amount: int
    total_fee_amount: int
    decayed_volatility_nad: int
    post_success_volatility_nad: int


@dataclass(frozen=True)
class PreliminarySwapInputs:
    amount_in_for_quote: int
    reserve_input_credit: int
    base_fee_amount: int
    volatility_surcharge_amount: int
    decayed_volatility_nad: int


@dataclass(frozen=True)
class SwapFeeAssessment:
    """Complete fee and reserve accounting before the final AMM quote."""

    reserve_credit: int
    base_fee_debit: int
    divergence_surcharge_debit: int
    volatility_surcharge_debit: int
    dynamic_surcharge_debit: int
    total_fee_debit: int
    retained_surcharge: int
    distributed_surcharge_debit: int
    amount_in_for_quote: int
    reserve_input_credit: int
    claimable_fee_debit: int
    base_fee_rate_nad: int
    divergence_fee_rate_nad: int
    volatility_fee_rate_nad: int
    total_fee_rate_nad: int
    decayed_volatility_nad: int

    def validate_accounting(self) -> None:
        if self.amount_in_for_quote + self.total_fee_debit != self.reserve_credit:
            raise ArithmeticError("quote input plus fees must equal gross input")
        if self.base_fee_debit + self.dynamic_surcharge_debit != self.total_fee_debit:
            raise ArithmeticError("base plus dynamic fees must equal total fee")
        if self.divergence_surcharge_debit + self.volatility_surcharge_debit != self.dynamic_surcharge_debit:
            raise ArithmeticError("dynamic fee components do not reconcile")
        if self.retained_surcharge + self.distributed_surcharge_debit != self.dynamic_surcharge_debit:
            raise ArithmeticError("dynamic fee disposition does not reconcile")
        if self.base_fee_debit + self.distributed_surcharge_debit != self.claimable_fee_debit:
            raise ArithmeticError("claimable fee does not reconcile")
        if self.amount_in_for_quote + self.retained_surcharge != self.reserve_input_credit:
            raise ArithmeticError("reserve input credit does not reconcile")
        if self.reserve_input_credit + self.claimable_fee_debit != self.reserve_credit:
            raise ArithmeticError("reserve plus claimable accounting does not reconcile")


def symmetric_ratio_distance_nad(a: int, b: int) -> int:
    _require_u64(a, "a")
    _require_u64(b, "b")
    if a == 0 or b == 0:
        raise ValueError("prices must be positive")
    high = max(a, b)
    low = min(a, b)
    return ceil_div(high * NAD, low) - NAD


def _divergence_state_potential_nad(
    outward_coordinate: int,
    k_squared: int,
    marginal_cap_rate_nad: int,
) -> int:
    if outward_coordinate == 0:
        return 0
    outward_squared = outward_coordinate * outward_coordinate
    saturation_ratio_nad = outward_squared * NAD // (outward_squared + k_squared)
    numerator = outward_coordinate * saturation_ratio_nad * 8 * marginal_cap_rate_nad
    return numerator // (9 * NAD * NAD)


def outward_divergence_fee_potential_nad(
    center_input_reserve: int,
    start_input_reserve: int,
    end_input_reserve: int,
    coefficient_nad: int,
    marginal_cap_rate_nad: int,
) -> int:
    """Additive outward-only potential over an AMM input-reserve coordinate."""

    for name, value in (
        ("center_input_reserve", center_input_reserve),
        ("start_input_reserve", start_input_reserve),
        ("end_input_reserve", end_input_reserve),
    ):
        _require_nonnegative_int(value, name)
    _require_u64(coefficient_nad, "coefficient_nad")
    _require_u64(marginal_cap_rate_nad, "marginal_cap_rate_nad")
    if center_input_reserve == 0:
        raise ValueError("center_input_reserve must be positive")
    if end_input_reserve < start_input_reserve:
        raise ValueError("exact-in input reserve cannot decrease")
    if marginal_cap_rate_nad > NAD:
        raise ValueError("marginal cap cannot exceed NAD")
    if coefficient_nad == 0 or marginal_cap_rate_nad == 0:
        return 0

    start_outward = max(start_input_reserve - center_input_reserve, 0)
    end_outward = max(end_input_reserve - center_input_reserve, 0)
    if end_outward <= start_outward:
        return 0

    k_squared = ceil_div(
        center_input_reserve * center_input_reserve * 2 * marginal_cap_rate_nad,
        coefficient_nad * 3,
    )
    start_potential = _divergence_state_potential_nad(
        start_outward, k_squared, marginal_cap_rate_nad
    )
    end_potential = _divergence_state_potential_nad(
        end_outward, k_squared, marginal_cap_rate_nad
    )
    return end_potential - start_potential


def decay_volatility_nad(
    accumulator_nad: int,
    last_update_slot: int,
    current_slot: int,
    half_life_ms: int,
) -> int:
    _require_u64(accumulator_nad, "accumulator_nad")
    _require_u64(last_update_slot, "last_update_slot")
    _require_u64(current_slot, "current_slot")
    _require_u64(half_life_ms, "half_life_ms")
    if accumulator_nad == 0:
        return 0
    if half_life_ms == 0:
        return 0
    if current_slot < last_update_slot:
        raise ValueError("current_slot cannot precede last_update_slot")
    elapsed_slots = current_slot - last_update_slot
    elapsed_ms = elapsed_slots * TARGET_MS_PER_SLOT
    if elapsed_ms > U64_MAX:
        raise OverflowError("elapsed milliseconds overflow u64")
    return exponential_decay_nad(accumulator_nad, elapsed_ms, half_life_ms)


def volatility_after_success_nad(
    decayed_accumulator_nad: int,
    start_price_nad: int,
    end_price_nad: int,
    shock_cap_nad: int,
    accumulator_cap_nad: int,
) -> int:
    for name, value in (
        ("decayed_accumulator_nad", decayed_accumulator_nad),
        ("start_price_nad", start_price_nad),
        ("end_price_nad", end_price_nad),
        ("shock_cap_nad", shock_cap_nad),
        ("accumulator_cap_nad", accumulator_cap_nad),
    ):
        _require_u64(value, name)
    if shock_cap_nad > accumulator_cap_nad:
        raise ValueError("shock cap cannot exceed accumulator cap")
    if start_price_nad == end_price_nad:
        return min(decayed_accumulator_nad, accumulator_cap_nad)
    shock_nad = min(
        symmetric_ratio_distance_nad(start_price_nad, end_price_nad),
        shock_cap_nad,
    )
    return min(decayed_accumulator_nad + shock_nad, accumulator_cap_nad)


def fee_amount_ceil(amount: int, fee_rate_nad: int) -> int:
    _require_u64(amount, "amount")
    _require_u64(fee_rate_nad, "fee_rate_nad")
    if fee_rate_nad > NAD:
        raise ValueError("fee rate cannot exceed NAD")
    fee = ceil_div(amount * fee_rate_nad, NAD)
    _require_u64(fee, "fee")
    return fee


def _scaled_rate_capped(signal_nad: int, coefficient_nad: int, cap_nad: int) -> int:
    _require_nonnegative_int(signal_nad, "signal_nad")
    _require_u64(coefficient_nad, "coefficient_nad")
    _require_u64(cap_nad, "cap_nad")
    if signal_nad == 0 or coefficient_nad == 0 or cap_nad == 0:
        return 0
    cap_threshold = ceil_div(cap_nad * NAD, coefficient_nad)
    if signal_nad >= cap_threshold:
        return cap_nad
    return min(ceil_div(signal_nad * coefficient_nad, NAD), cap_nad)


def _effective_rate_nad(amount: int, total_amount: int, cap_nad: int) -> int:
    if amount == 0 or cap_nad == 0:
        return 0
    return min(ceil_div(amount * NAD, total_amount), cap_nad)


def quote_dynamic_fee(
    config: DynamicFeeConfig,
    pre_state: DynamicFeePreState,
    path: DynamicFeePath,
) -> DynamicFeeQuote:
    """Quote base, divergence, and volatility fees with Dusk's priority."""

    config.validate()
    pre_state.validate()
    path.validate()
    decayed_volatility_nad = decay_volatility_nad(
        min(pre_state.volatility_accumulator_nad, config.volatility_accumulator_cap_nad),
        pre_state.volatility_last_update_slot,
        path.current_slot,
        config.volatility_half_life_ms,
    )
    dynamic_room_nad = config.max_fee_rate_nad - config.base_fee_rate_nad
    base_fee_amount = fee_amount_ceil(path.amount_in, config.base_fee_rate_nad)
    maximum_total_fee_amount = fee_amount_ceil(path.amount_in, config.max_fee_rate_nad)
    dynamic_room_amount = maximum_total_fee_amount - base_fee_amount

    divergence_amount = (
        0
        if config.divergence_coefficient_nad == 0
        else min(path.divergence_surcharge_amount, dynamic_room_amount)
    )
    remaining_dynamic_room_amount = dynamic_room_amount - divergence_amount
    requested_volatility_rate_nad = _scaled_rate_capped(
        decayed_volatility_nad,
        config.volatility_coefficient_nad,
        dynamic_room_nad,
    )
    requested_volatility_amount = fee_amount_ceil(
        path.amount_in, requested_volatility_rate_nad
    )
    volatility_amount = min(
        requested_volatility_amount, remaining_dynamic_room_amount
    )
    dynamic_amount = divergence_amount + volatility_amount
    total_fee_amount = base_fee_amount + dynamic_amount

    divergence_rate_nad = _effective_rate_nad(
        divergence_amount, path.amount_in, dynamic_room_nad
    )
    volatility_rate_nad = _effective_rate_nad(
        volatility_amount,
        path.amount_in,
        dynamic_room_nad - divergence_rate_nad,
    )
    total_rate_nad = min(
        config.base_fee_rate_nad + divergence_rate_nad + volatility_rate_nad,
        config.max_fee_rate_nad,
    )
    post_success_volatility_nad = volatility_after_success_nad(
        decayed_volatility_nad,
        path.start_price_nad,
        path.end_price_nad,
        config.volatility_shock_cap_nad,
        config.volatility_accumulator_cap_nad,
    )
    return DynamicFeeQuote(
        base_rate_nad=config.base_fee_rate_nad,
        divergence_rate_nad=divergence_rate_nad,
        volatility_rate_nad=volatility_rate_nad,
        total_rate_nad=total_rate_nad,
        base_fee_amount=base_fee_amount,
        divergence_surcharge_amount=divergence_amount,
        volatility_surcharge_amount=volatility_amount,
        dynamic_surcharge_amount=dynamic_amount,
        total_fee_amount=total_fee_amount,
        decayed_volatility_nad=decayed_volatility_nad,
        post_success_volatility_nad=post_success_volatility_nad,
    )


def preliminary_swap_inputs(
    config: DynamicFeeConfig,
    pre_state: DynamicFeePreState,
    reserve_credit: int,
    current_slot: int,
    retain_dynamic_surcharge: bool,
) -> PreliminarySwapInputs:
    """Deduct base plus known volatility before measuring divergence."""

    _require_u64(reserve_credit, "reserve_credit")
    _require_u64(current_slot, "current_slot")
    if reserve_credit == 0:
        raise ValueError("reserve_credit must be positive")
    preliminary_config = replace(config, divergence_coefficient_nad=0)
    preliminary = quote_dynamic_fee(
        preliminary_config,
        pre_state,
        DynamicFeePath(
            amount_in=reserve_credit,
            start_price_nad=pre_state.center_price_nad,
            end_price_nad=pre_state.center_price_nad,
            current_slot=current_slot,
            divergence_surcharge_amount=0,
        ),
    )
    amount_in_for_quote = reserve_credit - preliminary.total_fee_amount
    if amount_in_for_quote <= 0:
        raise ValueError("fees consume the entire input")
    reserve_input_credit = (
        reserve_credit - preliminary.base_fee_amount
        if retain_dynamic_surcharge
        else amount_in_for_quote
    )
    return PreliminarySwapInputs(
        amount_in_for_quote=amount_in_for_quote,
        reserve_input_credit=reserve_input_credit,
        base_fee_amount=preliminary.base_fee_amount,
        volatility_surcharge_amount=preliminary.volatility_surcharge_amount,
        decayed_volatility_nad=preliminary.decayed_volatility_nad,
    )


def assess_swap_fee(
    config: DynamicFeeConfig,
    pre_state: DynamicFeePreState,
    reserve_credit: int,
    current_slot: int,
    center_input_reserve: int,
    start_input_reserve: int,
    retain_dynamic_surcharge: bool,
) -> SwapFeeAssessment:
    """Assess a Dusk swap before the final invariant quote.

    ``center_input_reserve`` and ``start_input_reserve`` are the invariant's
    input-token coordinates in the same integer unit as ``reserve_credit``.
    The preliminary endpoint intentionally excludes divergence, matching the
    conservative on-chain two-pass algorithm.
    """

    preliminary = preliminary_swap_inputs(
        config,
        pre_state,
        reserve_credit,
        current_slot,
        retain_dynamic_surcharge,
    )
    dynamic_room_nad = config.max_fee_rate_nad - config.base_fee_rate_nad
    divergence_amount = outward_divergence_fee_potential_nad(
        center_input_reserve,
        start_input_reserve,
        start_input_reserve + preliminary.amount_in_for_quote,
        config.divergence_coefficient_nad,
        dynamic_room_nad,
    )
    dynamic = quote_dynamic_fee(
        config,
        pre_state,
        DynamicFeePath(
            amount_in=reserve_credit,
            # Fee calculation consumes the decayed pre-state only.  The actual
            # AMM spot shock is committed separately after a successful quote.
            start_price_nad=pre_state.center_price_nad,
            end_price_nad=pre_state.center_price_nad,
            current_slot=current_slot,
            divergence_surcharge_amount=divergence_amount,
        ),
    )
    amount_in_for_quote = reserve_credit - dynamic.total_fee_amount
    if amount_in_for_quote <= 0:
        raise ValueError("fees consume the entire input")
    if retain_dynamic_surcharge:
        retained_surcharge = dynamic.dynamic_surcharge_amount
        distributed_surcharge = 0
    else:
        retained_surcharge = 0
        distributed_surcharge = dynamic.dynamic_surcharge_amount
    claimable_fee = dynamic.base_fee_amount + distributed_surcharge
    reserve_input_credit = amount_in_for_quote + retained_surcharge

    assessment = SwapFeeAssessment(
        reserve_credit=reserve_credit,
        base_fee_debit=dynamic.base_fee_amount,
        divergence_surcharge_debit=dynamic.divergence_surcharge_amount,
        volatility_surcharge_debit=dynamic.volatility_surcharge_amount,
        dynamic_surcharge_debit=dynamic.dynamic_surcharge_amount,
        total_fee_debit=dynamic.total_fee_amount,
        retained_surcharge=retained_surcharge,
        distributed_surcharge_debit=distributed_surcharge,
        amount_in_for_quote=amount_in_for_quote,
        reserve_input_credit=reserve_input_credit,
        claimable_fee_debit=claimable_fee,
        base_fee_rate_nad=dynamic.base_rate_nad,
        divergence_fee_rate_nad=dynamic.divergence_rate_nad,
        volatility_fee_rate_nad=dynamic.volatility_rate_nad,
        total_fee_rate_nad=dynamic.total_rate_nad,
        decayed_volatility_nad=dynamic.decayed_volatility_nad,
    )
    assessment.validate_accounting()
    return assessment


def volatility_after_assessed_swap(
    config: DynamicFeeConfig,
    assessment: SwapFeeAssessment,
    actual_start_price_nad: int,
    actual_end_price_nad: int,
) -> int:
    """Return the accumulator to commit after a successful final AMM quote."""

    config.validate()
    assessment.validate_accounting()
    return volatility_after_success_nad(
        assessment.decayed_volatility_nad,
        actual_start_price_nad,
        actual_end_price_nad,
        config.volatility_shock_cap_nad,
        config.volatility_accumulator_cap_nad,
    )
