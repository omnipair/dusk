//! Standalone integer reference for the Dusk finite-C1 invariant.
//!
//! This crate is deliberately outside the on-chain workspace. It proves the
//! Q64/u128 geometry and raw-atom quote contract before production dispatch is
//! replaced. No big integer is used.

use core::cmp::Ordering;

pub const NAD: u128 = 1_000_000_000;
pub const Q48: u128 = 1_u128 << 48;
pub const Q64: u128 = 1_u128 << 64;
pub const MAX_SOLVER_ITERATIONS: usize = 65;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidArgument,
    Overflow,
    NonConvergence,
    UnsupportedState,
}

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeStart {
    Quarter,
    Half,
}

impl BridgeStart {
    const fn scale(self, value: u128) -> u128 {
        match self {
            Self::Quarter => value >> 2,
            Self::Half => value >> 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Region {
    Inner,
    Transition,
    Tail,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Geometry {
    pub q_start_q48: u128,
    pub q_tail_q48: u128,
    pub q_start_q64: u128,
    pub q_tail_q64: u128,
    pub v_start_q64: u128,
    pub v_tail_q64: u128,
    pub v_start_q48: u128,
    pub v_tail_q48: u128,
    pub reserve_ratio_start_q48: u128,
    pub reserve_ratio_tail_q48: u128,
    pub eta_start_q64: u128,
    pub eta_tail_q64: u128,
    pub negative_q_prime_start_q64: u128,
    pub negative_q_prime_start_q48: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Quote {
    pub amount_in: u64,
    pub amount_out: u64,
    pub reserve_in_after: u64,
    pub reserve_out_after: u64,
    pub region_before: Region,
    pub region_after: Region,
    pub iterations: SolverIterations,
    pub used_exact_tail: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SolverIterations {
    pub q_evaluations: u32,
    pub inner_q_iterations: u32,
    pub invariant_iterations: u32,
    pub reserve_iterations: u32,
}

impl SolverIterations {
    pub const fn total(self) -> u32 {
        self.inner_q_iterations + self.invariant_iterations + self.reserve_iterations
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SignedMagnitude {
    positive: bool,
    magnitude: u128,
}

fn imbalance_eta_q64(x: u64, y: u64) -> Result<u128> {
    if x == 0 || y == 0 {
        return Err(Error::InvalidArgument);
    }
    let difference = u128::from(x.abs_diff(y));
    let sum = (x as u128).checked_add(y as u128).ok_or(Error::Overflow)?;
    mul_div_floor(difference, Q64, sum)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FiniteC1 {
    peak_q64: u128,
    scale_q64: u128,
    bridge_start: BridgeStart,
    geometry: Geometry,
}

fn mul_div_rem(a: u128, b: u128, denominator: u128) -> Result<(u128, u128)> {
    if denominator == 0 {
        return Err(Error::InvalidArgument);
    }
    if a == 0 || b == 0 {
        return Ok((0, 0));
    }
    if let Some(product) = a.checked_mul(b) {
        return Ok((product / denominator, product % denominator));
    }

    let base = (a / denominator).checked_mul(b).ok_or(Error::Overflow)?;
    let addend = a % denominator;
    let mut quotient = 0_u128;
    let mut remainder = 0_u128;
    let significant_bits = u128::BITS - b.leading_zeros();
    for bit in (0..significant_bits).rev() {
        let carry = if remainder >= denominator - remainder {
            remainder -= denominator - remainder;
            1
        } else {
            remainder += remainder;
            0
        };
        quotient = quotient
            .checked_mul(2)
            .and_then(|value| value.checked_add(carry))
            .ok_or(Error::Overflow)?;

        if (b >> bit) & 1 == 1 {
            let carry = if remainder >= denominator - addend {
                remainder -= denominator - addend;
                1
            } else {
                remainder += addend;
                0
            };
            quotient = quotient.checked_add(carry).ok_or(Error::Overflow)?;
        }
    }
    Ok((
        base.checked_add(quotient).ok_or(Error::Overflow)?,
        remainder,
    ))
}

fn mul_div_floor(a: u128, b: u128, denominator: u128) -> Result<u128> {
    Ok(mul_div_rem(a, b, denominator)?.0)
}

fn mul_div_ceil(a: u128, b: u128, denominator: u128) -> Result<u128> {
    let (quotient, remainder) = mul_div_rem(a, b, denominator)?;
    quotient
        .checked_add(u128::from(remainder != 0))
        .ok_or(Error::Overflow)
}

fn mul_q48(a: u128, b: u128) -> Result<u128> {
    mul_div_floor(a, b, Q48)
}

fn mul_q64(a: u128, b: u128) -> Result<u128> {
    let (a_hi, a_lo) = (a >> 64, a & (Q64 - 1));
    let (b_hi, b_lo) = (b >> 64, b & (Q64 - 1));
    a_hi.checked_mul(b_hi)
        .and_then(|value| value.checked_mul(Q64))
        .and_then(|value| value.checked_add(a_hi.checked_mul(b_lo)?))
        .and_then(|value| value.checked_add(b_hi.checked_mul(a_lo)?))
        .and_then(|value| value.checked_add(a_lo.checked_mul(b_lo)? >> 64))
        .ok_or(Error::Overflow)
}

fn div_q64(numerator: u128, denominator: u128) -> Result<u128> {
    mul_div_floor(numerator, Q64, denominator)
}

fn to_q64_nad(value_nad: u128) -> Result<u128> {
    mul_div_floor(value_nad, Q64, NAD)
}

fn isqrt(value: u128) -> u128 {
    if value < 2 {
        return value;
    }
    let bits = u128::BITS - value.leading_zeros();
    let mut estimate = 1_u128 << bits.div_ceil(2);
    loop {
        let next = (estimate + value / estimate) >> 1;
        if next >= estimate {
            return estimate;
        }
        estimate = next;
    }
}

fn signed_difference(positive: u128, negative: u128) -> SignedMagnitude {
    match positive.cmp(&negative) {
        Ordering::Greater | Ordering::Equal => SignedMagnitude {
            positive: true,
            magnitude: positive - negative,
        },
        Ordering::Less => SignedMagnitude {
            positive: false,
            magnitude: negative - positive,
        },
    }
}

fn inner_residual_fixed(
    peak_q64: u128,
    scale_q64: u128,
    q_q64: u128,
    cosh_q64: u128,
) -> Result<SignedMagnitude> {
    if q_q64 == 0 || q_q64 > Q64 || cosh_q64 < Q64 {
        return Err(Error::InvalidArgument);
    }
    let delta = Q64 - q_q64;
    let denominator = scale_q64.checked_add(delta).ok_or(Error::Overflow)?;
    let weight_base = div_q64(scale_q64, denominator)?;
    let weight = mul_q64(weight_base, weight_base)?;
    let sqrt_q = if q_q64 == Q64 {
        Q64
    } else {
        isqrt(q_q64.checked_mul(Q64).ok_or(Error::Overflow)?)
    };
    let sqrt_q_cosh = mul_q64(sqrt_q, cosh_q64)?;
    let h = sqrt_q_cosh.abs_diff(Q64);
    let coefficient = mul_q64(
        peak_q64.checked_mul(2).ok_or(Error::Overflow)?,
        mul_q64(q_q64, weight)?,
    )?;
    let concentration = mul_q64(coefficient, h)?;
    let positive_concentration = u128::from(sqrt_q_cosh >= Q64)
        .checked_mul(concentration)
        .ok_or(Error::Overflow)?;
    let negative_concentration = u128::from(sqrt_q_cosh < Q64)
        .checked_mul(concentration)
        .ok_or(Error::Overflow)?;
    Ok(signed_difference(
        q_q64
            .checked_add(positive_concentration)
            .ok_or(Error::Overflow)?,
        Q64.checked_add(negative_concentration)
            .ok_or(Error::Overflow)?,
    ))
}

fn reserve_ratio_from_v_q48(v_q48: u128) -> Result<u128> {
    let cosh_q48 = isqrt(
        Q48.checked_mul(Q48)
            .and_then(|one_squared| {
                v_q48
                    .checked_mul(v_q48)
                    .and_then(|v_squared| one_squared.checked_add(v_squared))
            })
            .ok_or(Error::Overflow)?,
    );
    let exponential = cosh_q48.checked_add(v_q48).ok_or(Error::Overflow)?;
    mul_q48(exponential, exponential)
}

fn shape_from_eta_q64(eta_q64: u128) -> Result<(u128, u128)> {
    if eta_q64 >= Q64 {
        return Err(Error::InvalidArgument);
    }
    if eta_q64 == 0 {
        return Ok((0, Q64));
    }
    let eta_squared_q64 = mul_div_floor(eta_q64, eta_q64, Q64)?;
    if eta_squared_q64 == 0 {
        return Ok((eta_q64, Q64));
    }
    let inverse_cosh_squared_q64 = Q64.checked_sub(eta_squared_q64).ok_or(Error::Overflow)?;
    let inverse_cosh_q64 = isqrt(
        inverse_cosh_squared_q64
            .checked_mul(Q64)
            .ok_or(Error::Overflow)?,
    );
    let cosh_whole = Q64 / inverse_cosh_q64;
    let cosh_remainder = Q64 % inverse_cosh_q64;
    let cosh_q64 = cosh_whole
        .checked_mul(Q64)
        .and_then(|whole| {
            cosh_remainder
                .checked_mul(Q64)
                .and_then(|numerator| whole.checked_add(numerator / inverse_cosh_q64))
        })
        .ok_or(Error::Overflow)?;
    let cosh_hi = cosh_q64 >> 64;
    let cosh_lo = cosh_q64 & (Q64 - 1);
    let v_q64 = eta_q64
        .checked_mul(cosh_hi)
        .and_then(|whole| {
            eta_q64
                .checked_mul(cosh_lo)
                .and_then(|fractional| whole.checked_add(fractional >> 64))
        })
        .ok_or(Error::Overflow)?;
    Ok((v_q64, cosh_q64))
}

fn balance_factor_q64(x: u64, y: u64, invariant: u128) -> Result<u128> {
    if x == 0 || y == 0 || invariant == 0 {
        return Err(Error::InvalidArgument);
    }
    let twice_x = u128::from(x).checked_mul(2).ok_or(Error::Overflow)?;
    let twice_y = u128::from(y).checked_mul(2).ok_or(Error::Overflow)?;
    let (scaled_x, scaled_x_remainder) = mul_div_rem(twice_x, Q64, invariant)?;
    let (whole, whole_remainder) = mul_div_rem(scaled_x, twice_y, invariant)?;
    let (carried, _) = mul_div_rem(scaled_x_remainder, twice_y, invariant)?;
    whole
        .checked_add(
            whole_remainder
                .checked_add(carried)
                .ok_or(Error::Overflow)?
                / invariant,
        )
        .ok_or(Error::Overflow)
}

fn same_imbalance_side(x_before: u64, y_before: u64, x_after: u64, y_after: u64) -> bool {
    matches!(
        (x_before.cmp(&y_before), x_after.cmp(&y_after)),
        (Ordering::Less, Ordering::Less) | (Ordering::Greater, Ordering::Greater)
    )
}

impl FiniteC1 {
    pub fn new(
        peak_depth_nad: u128,
        fade_scale_nad: u128,
        bridge_start: BridgeStart,
    ) -> Result<Self> {
        if !(1..=2_000 * NAD).contains(&peak_depth_nad)
            || !(100..=199_000_000).contains(&fade_scale_nad)
            || fade_scale_nad > peak_depth_nad.saturating_mul(100)
        {
            return Err(Error::InvalidArgument);
        }
        let peak_q64 = to_q64_nad(peak_depth_nad)?;
        let scale_q64 = to_q64_nad(fade_scale_nad)?;
        let geometry = Self::derive_geometry(peak_q64, scale_q64, bridge_start)?;
        Ok(Self {
            peak_q64,
            scale_q64,
            bridge_start,
            geometry,
        })
    }

    pub const fn bridge_start(self) -> BridgeStart {
        self.bridge_start
    }

    pub const fn geometry(self) -> Geometry {
        self.geometry
    }

    fn inner_residual(self, q_q64: u128, cosh_q64: u128) -> Result<SignedMagnitude> {
        inner_residual_fixed(self.peak_q64, self.scale_q64, q_q64, cosh_q64)
    }

    fn solve_inner_q(self, cosh_q64: u128) -> Result<(u128, u32)> {
        if cosh_q64 == Q64 {
            return Ok((Q64, 0));
        }
        let mut low = 1_u128;
        let mut high = Q64;
        if self.inner_residual(low, cosh_q64)?.positive
            || !self.inner_residual(high, cosh_q64)?.positive
        {
            return Err(Error::UnsupportedState);
        }
        for iteration in 1..=64 {
            if high - low <= 1 {
                return Ok((high, iteration - 1));
            }
            let middle = low + (high - low) / 2;
            if self.inner_residual(middle, cosh_q64)?.positive {
                high = middle;
            } else {
                low = middle;
            }
        }
        (high - low <= 1)
            .then_some((high, 64))
            .ok_or(Error::NonConvergence)
    }

    fn derive_geometry(
        peak_q64: u128,
        scale_q64: u128,
        bridge_start: BridgeStart,
    ) -> Result<Geometry> {
        let nominal_delta_start = bridge_start.scale(scale_q64);
        let nominal_q_start = Q64
            .checked_sub(nominal_delta_start)
            .ok_or(Error::Overflow)?;
        let q_tail = Q64.checked_sub(scale_q64).ok_or(Error::Overflow)?;
        let nominal_weight_base = div_q64(
            scale_q64,
            scale_q64
                .checked_add(nominal_delta_start)
                .ok_or(Error::Overflow)?,
        )?;
        let nominal_weight = mul_q64(nominal_weight_base, nominal_weight_base)?;
        let nominal_coefficient = mul_q64(
            peak_q64.checked_mul(2).ok_or(Error::Overflow)?,
            mul_q64(nominal_q_start, nominal_weight)?,
        )?;
        if nominal_coefficient == 0 {
            return Err(Error::UnsupportedState);
        }
        let nominal_h = div_q64(nominal_delta_start, nominal_coefficient)?;
        let nominal_sqrt_q = isqrt(nominal_q_start.checked_mul(Q64).ok_or(Error::Overflow)?);
        let provisional_cosh_start = div_q64(
            Q64.checked_add(nominal_h).ok_or(Error::Overflow)?,
            nominal_sqrt_q,
        )?;
        let inverse_cosh_start = div_q64(Q64, provisional_cosh_start)?;
        let eta_start_squared = Q64
            .checked_sub(mul_q64(inverse_cosh_start, inverse_cosh_start)?)
            .ok_or(Error::Overflow)?;
        let eta_start_q64 = isqrt(eta_start_squared.checked_mul(Q64).ok_or(Error::Overflow)?);
        let (v_start, cosh_start) = shape_from_eta_q64(eta_start_q64)?;

        // The continuous construction chooses nominal_q_start. Q64 rounding
        // means that pair (nominal_q_start, v_start) is not necessarily an
        // integer root. Anchor the transition to the canonical inner root at
        // the one shared discrete boundary instead of switching formulas at a
        // nearby nominal value.
        let mut q_start = nominal_q_start;
        if inner_residual_fixed(peak_q64, scale_q64, q_start, cosh_start)?.positive {
            for _ in 0..64 {
                let predecessor = q_start.checked_sub(1).ok_or(Error::UnsupportedState)?;
                if !inner_residual_fixed(peak_q64, scale_q64, predecessor, cosh_start)?.positive {
                    break;
                }
                q_start = predecessor;
            }
        } else {
            for _ in 0..64 {
                q_start = q_start.checked_add(1).ok_or(Error::UnsupportedState)?;
                if inner_residual_fixed(peak_q64, scale_q64, q_start, cosh_start)?.positive {
                    break;
                }
            }
        }
        if q_start <= q_tail
            || !inner_residual_fixed(peak_q64, scale_q64, q_start, cosh_start)?.positive
            || inner_residual_fixed(peak_q64, scale_q64, q_start - 1, cosh_start)?.positive
        {
            return Err(Error::UnsupportedState);
        }
        let delta_start = Q64.checked_sub(q_start).ok_or(Error::Overflow)?;

        let weight_base = div_q64(
            scale_q64,
            scale_q64.checked_add(delta_start).ok_or(Error::Overflow)?,
        )?;
        let weight = mul_q64(weight_base, weight_base)?;
        let coefficient = mul_q64(
            peak_q64.checked_mul(2).ok_or(Error::Overflow)?,
            mul_q64(q_start, weight)?,
        )?;
        let sqrt_q = isqrt(q_start.checked_mul(Q64).ok_or(Error::Overflow)?);
        let h_start = mul_q64(sqrt_q, cosh_start)?.abs_diff(Q64);

        let inverse_q = div_q64(Q64, q_start)?;
        let two_over_denominator = div_q64(
            Q64.checked_mul(2).ok_or(Error::Overflow)?,
            scale_q64.checked_add(delta_start).ok_or(Error::Overflow)?,
        )?;
        let coefficient_q = mul_q64(
            coefficient,
            inverse_q
                .checked_add(two_over_denominator)
                .ok_or(Error::Overflow)?,
        )?;
        let residual_q = mul_q64(coefficient_q, h_start)?
            .checked_add(div_q64(
                mul_q64(coefficient, cosh_start)?,
                sqrt_q.checked_mul(2).ok_or(Error::Overflow)?,
            )?)
            .and_then(|value| value.checked_add(Q64))
            .ok_or(Error::Overflow)?;
        let residual_v = div_q64(mul_q64(mul_q64(coefficient, sqrt_q)?, v_start)?, cosh_start)?;
        let negative_q_prime = div_q64(residual_v, residual_q)?;
        if negative_q_prime == 0 {
            return Err(Error::UnsupportedState);
        }
        // eta=v/cosh(v)=|x-y|/(x+y) is used only for the two branch
        // comparisons. The bridge itself stays in v so every configured
        // profile has an unbounded coordinate domain and preserves the
        // analytical first derivative chosen at the inner join.
        let bridge_drop = q_start.checked_sub(q_tail).ok_or(Error::Overflow)?;
        let bridge_length = div_q64(
            bridge_drop.checked_mul(3).ok_or(Error::Overflow)?,
            negative_q_prime,
        )?;
        let v_tail = v_start.checked_add(bridge_length).ok_or(Error::Overflow)?;
        let v_tail_squared = mul_q64(v_tail, v_tail)?;
        let eta_tail_squared = div_q64(
            v_tail_squared,
            Q64.checked_add(v_tail_squared).ok_or(Error::Overflow)?,
        )?;
        let eta_tail_q64 = isqrt(eta_tail_squared.checked_mul(Q64).ok_or(Error::Overflow)?);
        Ok(Geometry {
            q_start_q48: q_start >> 16,
            q_tail_q48: q_tail >> 16,
            q_start_q64: q_start,
            q_tail_q64: q_tail,
            v_start_q64: v_start,
            v_tail_q64: v_tail,
            v_start_q48: v_start >> 16,
            v_tail_q48: v_tail >> 16,
            reserve_ratio_start_q48: reserve_ratio_from_v_q48(v_start >> 16)?,
            reserve_ratio_tail_q48: reserve_ratio_from_v_q48(v_tail >> 16)?,
            eta_start_q64,
            eta_tail_q64,
            negative_q_prime_start_q64: negative_q_prime,
            negative_q_prime_start_q48: negative_q_prime >> 16,
        })
    }

    fn transition_q(self, v_q64: u128) -> Result<u128> {
        let z = mul_div_floor(
            v_q64
                .checked_sub(self.geometry.v_start_q64)
                .ok_or(Error::InvalidArgument)?,
            Q64,
            self.geometry.v_tail_q64 - self.geometry.v_start_q64,
        )?
        .min(Q64);
        let one_minus_z = Q64 - z;
        let one_minus_z_squared = mul_q64(one_minus_z, one_minus_z)?;
        let one_minus_z_cubed = mul_q64(one_minus_z_squared, one_minus_z)?;
        self.geometry
            .q_tail_q64
            .checked_add(mul_q64(
                self.geometry.q_start_q64 - self.geometry.q_tail_q64,
                one_minus_z_cubed,
            )?)
            .ok_or(Error::Overflow)
    }

    pub fn q_for_v_q48(self, v_q48: u128) -> Result<(u128, Region, u32)> {
        if v_q48 >= self.geometry.v_tail_q48 {
            return Ok((self.geometry.q_tail_q64, Region::Tail, 0));
        }
        if v_q48 == self.geometry.v_start_q48 {
            return Ok((self.geometry.q_start_q64, Region::Transition, 0));
        }
        if v_q48 >= self.geometry.v_start_q48 {
            return Ok((
                self.transition_q(v_q48.checked_shl(16).ok_or(Error::Overflow)?)?,
                Region::Transition,
                0,
            ));
        }
        let cosh_q48 = isqrt(
            Q48.checked_mul(Q48)
                .and_then(|one_squared| {
                    v_q48
                        .checked_mul(v_q48)
                        .and_then(|v_squared| one_squared.checked_add(v_squared))
                })
                .ok_or(Error::Overflow)?,
        );
        let (q_q64, iterations) =
            self.solve_inner_q(cosh_q48.checked_shl(16).ok_or(Error::Overflow)?)?;
        Ok((q_q64, Region::Inner, iterations))
    }

    fn q_for_reserves_counted(
        self,
        x: u64,
        y: u64,
        iterations: &mut SolverIterations,
    ) -> Result<(u128, Region)> {
        iterations.q_evaluations = iterations
            .q_evaluations
            .checked_add(1)
            .ok_or(Error::Overflow)?;
        let eta_q64 = imbalance_eta_q64(x, y)?;
        if eta_q64 >= self.geometry.eta_tail_q64 {
            return Ok((self.geometry.q_tail_q64, Region::Tail));
        }
        let (v_q64, cosh_q64) = shape_from_eta_q64(eta_q64)?;
        if eta_q64 < self.geometry.eta_start_q64 {
            let (q_q64, count) = self.solve_inner_q(cosh_q64)?;
            iterations.inner_q_iterations = iterations
                .inner_q_iterations
                .checked_add(count)
                .ok_or(Error::Overflow)?;
            return Ok((q_q64, Region::Inner));
        }
        Ok((
            self.transition_q(v_q64.clamp(self.geometry.v_start_q64, self.geometry.v_tail_q64))?,
            Region::Transition,
        ))
    }

    pub fn q_for_reserves(self, x: u64, y: u64) -> Result<(u128, Region)> {
        self.q_for_reserves_counted(x, y, &mut SolverIterations::default())
    }

    fn invariant_for_target(
        self,
        x: u64,
        y: u64,
        target_q: u128,
        iterations: &mut SolverIterations,
    ) -> Result<u128> {
        let product = (x as u128).checked_mul(y as u128).ok_or(Error::Overflow)?;
        let mut low = isqrt(product).checked_mul(2).ok_or(Error::Overflow)?;
        let mut high = (x as u128).checked_add(y as u128).ok_or(Error::Overflow)?;
        if low == high {
            return Ok(high);
        }
        for _ in 0..MAX_SOLVER_ITERATIONS {
            if high - low <= 1 {
                return Ok(high);
            }
            iterations.invariant_iterations = iterations
                .invariant_iterations
                .checked_add(1)
                .ok_or(Error::Overflow)?;
            let middle = low + (high - low) / 2;
            if balance_factor_q64(x, y, middle)? <= target_q {
                high = middle;
            } else {
                low = middle;
            }
        }
        Err(Error::NonConvergence)
    }

    pub fn invariant(self, x: u64, y: u64) -> Result<u128> {
        let mut iterations = SolverIterations::default();
        let (target_q, _) = self.q_for_reserves_counted(x, y, &mut iterations)?;
        self.invariant_for_target(x, y, target_q, &mut iterations)
    }

    fn reserve_is_valid_counted(
        self,
        x: u64,
        y: u64,
        invariant: u128,
        iterations: &mut SolverIterations,
    ) -> Result<bool> {
        let (target_q, _) = self.q_for_reserves_counted(x, y, iterations)?;
        Ok(balance_factor_q64(x, y, invariant)? >= target_q)
    }

    #[cfg(test)]
    fn reserve_is_valid(self, x: u64, y: u64, invariant: u128) -> Result<bool> {
        self.reserve_is_valid_counted(x, y, invariant, &mut SolverIterations::default())
    }

    pub fn quote_exact_in(self, x: u64, y: u64, amount_in: u64) -> Result<Quote> {
        if x == 0 || y <= 1 || amount_in == 0 {
            return Err(Error::InvalidArgument);
        }
        let mut iterations = SolverIterations::default();
        let (target_q, region_before) = self.q_for_reserves_counted(x, y, &mut iterations)?;
        let x_after = x.checked_add(amount_in).ok_or(Error::Overflow)?;
        if region_before == Region::Tail {
            let product = (x as u128).checked_mul(y as u128).ok_or(Error::Overflow)?;
            let tail_y_after = u64::try_from(mul_div_ceil(product, 1, x_after as u128)?)
                .map_err(|_| Error::Overflow)?;
            if tail_y_after < y {
                let region_after = self
                    .q_for_reserves_counted(x_after, tail_y_after, &mut iterations)?
                    .1;
                if region_after == Region::Tail && same_imbalance_side(x, y, x_after, tail_y_after)
                {
                    return Ok(Quote {
                        amount_in,
                        amount_out: y - tail_y_after,
                        reserve_in_after: x_after,
                        reserve_out_after: tail_y_after,
                        region_before,
                        region_after,
                        iterations,
                        used_exact_tail: true,
                    });
                }
            }
        }
        let invariant = self.invariant_for_target(x, y, target_q, &mut iterations)?;
        if !self.reserve_is_valid_counted(x_after, y, invariant, &mut iterations)? {
            return Err(Error::UnsupportedState);
        }
        let mut low = 1_u64;
        let mut high = y;
        if self.reserve_is_valid_counted(x_after, low, invariant, &mut iterations)? {
            high = low;
        } else {
            for _ in 0..MAX_SOLVER_ITERATIONS {
                if high - low <= 1 {
                    break;
                }
                iterations.reserve_iterations = iterations
                    .reserve_iterations
                    .checked_add(1)
                    .ok_or(Error::Overflow)?;
                let middle = low + (high - low) / 2;
                if self.reserve_is_valid_counted(x_after, middle, invariant, &mut iterations)? {
                    high = middle;
                } else {
                    low = middle;
                }
            }
            if high - low > 1 {
                return Err(Error::NonConvergence);
            }
        }
        let amount_out = y.checked_sub(high).ok_or(Error::Overflow)?;
        if amount_out == 0 {
            return Err(Error::UnsupportedState);
        }
        let region_after = self
            .q_for_reserves_counted(x_after, high, &mut iterations)?
            .1;
        Ok(Quote {
            amount_in,
            amount_out,
            reserve_in_after: x_after,
            reserve_out_after: high,
            region_before,
            region_after,
            iterations,
            used_exact_tail: false,
        })
    }

    pub fn quote_exact_out(self, x: u64, y: u64, amount_out: u64) -> Result<Quote> {
        if x == 0 || y <= amount_out || amount_out == 0 {
            return Err(Error::InvalidArgument);
        }
        let mut iterations = SolverIterations::default();
        let (target_q, region_before) = self.q_for_reserves_counted(x, y, &mut iterations)?;
        let y_after = y - amount_out;
        if region_before == Region::Tail {
            let product = (x as u128).checked_mul(y as u128).ok_or(Error::Overflow)?;
            let tail_x_after = u64::try_from(mul_div_ceil(product, 1, y_after as u128)?)
                .map_err(|_| Error::Overflow)?;
            if tail_x_after > x {
                let region_after = self
                    .q_for_reserves_counted(tail_x_after, y_after, &mut iterations)?
                    .1;
                if region_after == Region::Tail && same_imbalance_side(x, y, tail_x_after, y_after)
                {
                    return Ok(Quote {
                        amount_in: tail_x_after - x,
                        amount_out,
                        reserve_in_after: tail_x_after,
                        reserve_out_after: y_after,
                        region_before,
                        region_after,
                        iterations,
                        used_exact_tail: true,
                    });
                }
            }
        }
        let invariant = self.invariant_for_target(x, y, target_q, &mut iterations)?;
        let mut low = x;
        let mut high = u64::MAX;
        if self.reserve_is_valid_counted(low, y_after, invariant, &mut iterations)? {
            return Err(Error::UnsupportedState);
        }
        if !self.reserve_is_valid_counted(high, y_after, invariant, &mut iterations)? {
            return Err(Error::UnsupportedState);
        }
        for _ in 0..MAX_SOLVER_ITERATIONS {
            if high - low <= 1 {
                break;
            }
            iterations.reserve_iterations = iterations
                .reserve_iterations
                .checked_add(1)
                .ok_or(Error::Overflow)?;
            let middle = low + (high - low) / 2;
            if self.reserve_is_valid_counted(middle, y_after, invariant, &mut iterations)? {
                high = middle;
            } else {
                low = middle;
            }
        }
        if high - low > 1 {
            return Err(Error::NonConvergence);
        }
        let amount_in = high.checked_sub(x).ok_or(Error::Overflow)?;
        if amount_in == 0 {
            return Err(Error::UnsupportedState);
        }
        let region_after = self
            .q_for_reserves_counted(high, y_after, &mut iterations)?
            .1;
        Ok(Quote {
            amount_in,
            amount_out,
            reserve_in_after: high,
            reserve_out_after: y_after,
            region_before,
            region_after,
            iterations,
            used_exact_tail: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILES: [(u128, u128); 3] = [
        (20 * NAD, 50_000_000),
        (200 * NAD, 5_000_000),
        (2_000 * NAD, 500_000),
    ];

    fn state_for_ratio(ratio_q48: u128, high_first: bool) -> (u64, u64) {
        let low = 1_000_000_000_000_u64;
        let high = u64::try_from(mul_div_ceil(low as u128, ratio_q48, Q48).unwrap()).unwrap();
        if high_first {
            (high, low)
        } else {
            (low, high)
        }
    }

    fn region_rank(region: Region) -> u8 {
        match region {
            Region::Inner => 0,
            Region::Transition => 1,
            Region::Tail => 2,
        }
    }

    fn quote_crossing(model: FiniteC1, x: u64, y: u64, target: Region, outward: bool) -> Quote {
        let reached = |region| {
            if outward {
                region_rank(region) >= region_rank(target)
            } else {
                region_rank(region) <= region_rank(target)
            }
        };
        let mut low = 0_u64;
        let mut high = 1_u64;
        loop {
            if let Ok(quote) = model.quote_exact_in(x, y, high) {
                if reached(quote.region_after) {
                    break;
                }
            }
            low = high;
            high = high.checked_mul(2).expect("crossing amount overflowed");
        }
        while high - low > 1 {
            let middle = low + (high - low) / 2;
            let is_reached = model
                .quote_exact_in(x, y, middle)
                .map(|quote| reached(quote.region_after))
                .unwrap_or(false);
            if is_reached {
                high = middle;
            } else {
                low = middle;
            }
        }
        model.quote_exact_in(x, y, high).unwrap()
    }

    #[test]
    fn geometry_is_ordered_for_both_protocol_candidates() {
        for (peak, scale) in PROFILES {
            for bridge in [BridgeStart::Quarter, BridgeStart::Half] {
                let model = FiniteC1::new(peak, scale, bridge)
                    .unwrap_or_else(|error| panic!("profile failed: peak={peak} scale={scale} bridge={bridge:?} error={error:?}"));
                let geometry = model.geometry();
                assert!(geometry.q_tail_q48 < geometry.q_start_q48);
                assert!(geometry.q_start_q48 < Q48);
                assert!(geometry.v_start_q48 < geometry.v_tail_q48);
                assert!(geometry.negative_q_prime_start_q48 > 0);
            }
        }
    }

    #[test]
    fn overflowing_mul_div_is_exact_or_fails_closed() {
        assert_eq!(
            mul_div_rem(u128::MAX, u128::MAX, u128::MAX),
            Ok((u128::MAX, 0))
        );
        assert_eq!(
            mul_div_rem(u128::MAX, u128::MAX - 1, u128::MAX),
            Ok((u128::MAX - 1, 0))
        );
        assert_eq!(mul_div_rem(u128::MAX, 2, u128::MAX), Ok((2, 0)));
        assert_eq!(
            mul_div_rem(u128::MAX, u128::MAX, u128::MAX - 1),
            Err(Error::Overflow)
        );
    }

    #[test]
    fn q_and_discrete_slope_join_continuously_for_both_candidates() {
        for (peak, scale) in PROFILES {
            for bridge in [BridgeStart::Quarter, BridgeStart::Half] {
                let model = FiniteC1::new(peak, scale, bridge).unwrap();
                let geometry = model.geometry();
                let length = geometry.v_tail_q48 - geometry.v_start_q48;
                let start_step = (length / 10_000).max(1);
                let tail_step = (length / 100).max(1);

                let (q_start, start_region, _) = model.q_for_v_q48(geometry.v_start_q48).unwrap();
                let (q_tail, tail_region, _) = model.q_for_v_q48(geometry.v_tail_q48).unwrap();
                assert_eq!(
                    (q_start, start_region),
                    (geometry.q_start_q64, Region::Transition)
                );
                assert_eq!((q_tail, tail_region), (geometry.q_tail_q64, Region::Tail));

                let (q_before_start, before_region, _) = model
                    .q_for_v_q48(geometry.v_start_q48 - start_step)
                    .unwrap();
                let (q_after_start, after_region, _) = model
                    .q_for_v_q48(geometry.v_start_q48 + start_step)
                    .unwrap();
                assert_eq!(before_region, Region::Inner);
                assert_eq!(after_region, Region::Transition);
                let left_drop = q_before_start - q_start;
                let right_drop = q_start - q_after_start;
                let larger = left_drop.max(right_drop);
                assert!(
                    left_drop.abs_diff(right_drop) <= larger / 8 + (8 << 16),
                    "inner join slope mismatch: peak={peak} scale={scale} bridge={bridge:?} left={left_drop} right={right_drop}"
                );

                let (q_before_tail, before_tail_region, _) =
                    model.q_for_v_q48(geometry.v_tail_q48 - tail_step).unwrap();
                assert_eq!(before_tail_region, Region::Transition);
                let bridge_drop = geometry.q_start_q64 - geometry.q_tail_q64;
                assert!(q_before_tail >= q_tail);
                assert!(
                    q_before_tail - q_tail <= bridge_drop / 500_000 + (8 << 16),
                    "tail join did not flatten: peak={peak} scale={scale} bridge={bridge:?}"
                );
            }
        }
    }

    #[test]
    fn all_four_region_crossings_are_executable() {
        for (peak, scale) in PROFILES {
            for bridge in [BridgeStart::Quarter, BridgeStart::Half] {
                let model = FiniteC1::new(peak, scale, bridge).unwrap();
                let geometry = model.geometry();
                let ratio_gap = geometry.reserve_ratio_tail_q48 - geometry.reserve_ratio_start_q48;
                let inner_ratio = geometry.reserve_ratio_start_q48
                    - (geometry.reserve_ratio_start_q48 - Q48) / 100;
                let transition_near_start = geometry.reserve_ratio_start_q48 + ratio_gap / 100;
                let transition_near_tail = geometry.reserve_ratio_tail_q48 - ratio_gap / 100;
                let tail_ratio = geometry.reserve_ratio_tail_q48 + ratio_gap / 100;

                let (x, y) = state_for_ratio(inner_ratio, true);
                let inner_to_transition = quote_crossing(model, x, y, Region::Transition, true);
                assert_eq!(inner_to_transition.region_before, Region::Inner);
                assert_eq!(inner_to_transition.region_after, Region::Transition);

                let (x, y) = state_for_ratio(transition_near_tail, true);
                let transition_to_tail = quote_crossing(model, x, y, Region::Tail, true);
                assert_eq!(transition_to_tail.region_before, Region::Transition);
                assert_eq!(transition_to_tail.region_after, Region::Tail);

                let (x, y) = state_for_ratio(tail_ratio, false);
                let tail_to_transition = quote_crossing(model, x, y, Region::Transition, false);
                assert_eq!(tail_to_transition.region_before, Region::Tail);
                assert_eq!(tail_to_transition.region_after, Region::Transition);

                let (x, y) = state_for_ratio(transition_near_start, false);
                let transition_to_inner = quote_crossing(model, x, y, Region::Inner, false);
                assert_eq!(transition_to_inner.region_before, Region::Transition);
                assert_eq!(transition_to_inner.region_after, Region::Inner);
            }
        }
    }

    #[test]
    fn exact_tail_is_bit_identical_to_integer_cpmm_in_both_directions() {
        for (peak, scale) in PROFILES {
            for bridge in [BridgeStart::Quarter, BridgeStart::Half] {
                let model = FiniteC1::new(peak, scale, bridge).unwrap();
                let geometry = model.geometry();
                let tail_ratio =
                    geometry.reserve_ratio_tail_q48 + (geometry.reserve_ratio_tail_q48 - Q48);
                for high_first in [true, false] {
                    let (x, y) = state_for_ratio(tail_ratio, high_first);
                    let amount_in = x / 1_000;
                    let quote = model.quote_exact_in(x, y, amount_in).unwrap();
                    let expected_y = u64::try_from(
                        mul_div_ceil((x as u128) * (y as u128), 1, quote.reserve_in_after as u128)
                            .unwrap(),
                    )
                    .unwrap();
                    assert!(quote.used_exact_tail);
                    assert_eq!(quote.reserve_out_after, expected_y);
                    assert_eq!(quote.amount_out, y - expected_y);
                    assert_eq!(quote.iterations.total(), 0);

                    let exact_out = model.quote_exact_out(x, y, quote.amount_out).unwrap();
                    assert!(exact_out.used_exact_tail);
                    let expected_x = u64::try_from(
                        mul_div_ceil((x as u128) * (y as u128), 1, (y - quote.amount_out) as u128)
                            .unwrap(),
                    )
                    .unwrap();
                    assert_eq!(exact_out.amount_in, expected_x - x);
                    assert!(exact_out.amount_in <= quote.amount_in);
                    assert_eq!(exact_out.iterations.total(), 0);

                    let product_before = (x as u128) * (y as u128);
                    let product_after =
                        (quote.reserve_in_after as u128) * (quote.reserve_out_after as u128);
                    assert!(product_after >= product_before);
                    assert!(product_after - product_before < quote.reserve_in_after as u128);
                }
            }
        }
    }

    #[test]
    fn quotes_are_symmetric_and_exact_out_is_conservative() {
        for (peak, scale) in PROFILES {
            for bridge in [BridgeStart::Quarter, BridgeStart::Half] {
                let model = FiniteC1::new(peak, scale, bridge).unwrap();
                let geometry = model.geometry();
                for ratio in [
                    (Q48 + geometry.reserve_ratio_start_q48) / 2,
                    (geometry.reserve_ratio_start_q48 + geometry.reserve_ratio_tail_q48) / 2,
                    geometry.reserve_ratio_tail_q48 + (geometry.reserve_ratio_tail_q48 - Q48),
                ] {
                    let (x, y) = state_for_ratio(ratio, true);
                    assert_eq!(
                        model.q_for_reserves(x, y).unwrap(),
                        model.q_for_reserves(y, x).unwrap()
                    );
                    assert_eq!(
                        model.invariant(x, y).unwrap(),
                        model.invariant(y, x).unwrap()
                    );
                }
                for amount in [100_000_u64, 1_000_000, 10_000_000, 100_000_000] {
                    let forward = model
                        .quote_exact_in(1_000_000_000, 1_000_000_000, amount)
                        .unwrap();
                    let reverse = model
                        .quote_exact_in(1_000_000_000, 1_000_000_000, amount)
                        .unwrap();
                    assert_eq!(forward.amount_out, reverse.amount_out);
                    let inverse = model
                        .quote_exact_out(1_000_000_000, 1_000_000_000, forward.amount_out)
                        .unwrap();
                    assert!(inverse.amount_in >= amount);
                    assert!(inverse.amount_in - amount <= 2);
                }
            }
        }
    }

    #[test]
    fn output_is_maximal_on_the_integer_valid_side() {
        let model = FiniteC1::new(200 * NAD, 5_000_000, BridgeStart::Half).unwrap();
        for amount in [100_000_u64, 1_000_000, 10_000_000, 100_000_000, 500_000_000] {
            let quote = model
                .quote_exact_in(1_000_000_000, 1_000_000_000, amount)
                .unwrap();
            let invariant = model.invariant(1_000_000_000, 1_000_000_000).unwrap();
            assert!(model
                .reserve_is_valid(quote.reserve_in_after, quote.reserve_out_after, invariant)
                .unwrap());
            if quote.reserve_out_after > 1 {
                assert!(!model
                    .reserve_is_valid(
                        quote.reserve_in_after,
                        quote.reserve_out_after - 1,
                        invariant,
                    )
                    .unwrap());
            }
        }
    }

    #[test]
    fn exact_in_is_maximal_and_exact_out_input_is_minimal_by_one_atom() {
        for (peak, scale) in PROFILES {
            for bridge in [BridgeStart::Quarter, BridgeStart::Half] {
                let model = FiniteC1::new(peak, scale, bridge).unwrap();
                for (x, y) in [
                    (1_000_000_000_u64, 1_000_000_000_u64),
                    state_for_ratio(model.geometry().reserve_ratio_start_q48 + 10_000, true),
                    state_for_ratio(model.geometry().reserve_ratio_tail_q48 + 10_000, false),
                ] {
                    let exact_out = model.quote_exact_out(x, y, 1).unwrap();
                    let enough = model.quote_exact_in(x, y, exact_out.amount_in).unwrap();
                    assert!(enough.amount_out >= 1);
                    if exact_out.amount_in > 1 {
                        match model.quote_exact_in(x, y, exact_out.amount_in - 1) {
                            Ok(short) => assert!(short.amount_out < 1),
                            Err(Error::UnsupportedState) => {}
                            Err(error) => panic!("unexpected quote error: {error:?}"),
                        }
                    }

                    let exact_in = model.quote_exact_in(x, y, x / 1_000 + 1).unwrap();
                    if exact_in.amount_out < y - 1 {
                        let next = model
                            .quote_exact_out(x, y, exact_in.amount_out + 1)
                            .unwrap();
                        assert!(next.amount_in > exact_in.amount_in);
                    }
                }
            }
        }
    }

    #[test]
    fn split_trades_and_round_trips_cannot_create_atoms() {
        for (peak, scale) in PROFILES {
            for bridge in [BridgeStart::Quarter, BridgeStart::Half] {
                let model = FiniteC1::new(peak, scale, bridge).unwrap();
                let geometry = model.geometry();
                let scenarios = [
                    (1_000_000_000_000_u64, 1_000_000_000_000_u64),
                    state_for_ratio(
                        (geometry.reserve_ratio_start_q48 + geometry.reserve_ratio_tail_q48) / 2,
                        true,
                    ),
                    state_for_ratio(
                        geometry.reserve_ratio_tail_q48 + (geometry.reserve_ratio_tail_q48 - Q48),
                        false,
                    ),
                ];
                for (x, y) in scenarios {
                    let amount = x / 100;
                    let whole = model.quote_exact_in(x, y, amount).unwrap();
                    let first = model.quote_exact_in(x, y, amount / 2).unwrap();
                    let second = model
                        .quote_exact_in(
                            first.reserve_in_after,
                            first.reserve_out_after,
                            amount - amount / 2,
                        )
                        .unwrap();
                    let split_output = first.amount_out + second.amount_out;
                    assert!(
                        split_output <= whole.amount_out,
                        "split gained atoms: peak={peak} scale={scale} bridge={bridge:?} x={x} y={y} amount={amount} whole={} split={split_output}",
                        whole.amount_out
                    );
                    assert!(whole.amount_out - split_output <= 2);

                    let reverse = model
                        .quote_exact_in(
                            whole.reserve_out_after,
                            whole.reserve_in_after,
                            whole.amount_out,
                        )
                        .unwrap();
                    assert!(reverse.amount_out <= amount);
                }
            }
        }
    }

    #[test]
    fn parameter_and_reserve_extremes_fail_closed_without_overflow() {
        assert_eq!(
            FiniteC1::new(0, 100, BridgeStart::Quarter),
            Err(Error::InvalidArgument)
        );
        assert!(FiniteC1::new(1, 100, BridgeStart::Quarter).is_ok());
        assert_eq!(
            FiniteC1::new(2 * NAD, 99, BridgeStart::Quarter),
            Err(Error::InvalidArgument)
        );
        assert_eq!(
            FiniteC1::new(2_000 * NAD + 1, 199_000_000, BridgeStart::Half),
            Err(Error::InvalidArgument)
        );
        assert_eq!(
            FiniteC1::new(2_000 * NAD, 199_000_001, BridgeStart::Half),
            Err(Error::InvalidArgument)
        );

        for (peak, scale) in [
            (2 * NAD, 100),
            (2 * NAD, 199_000_000),
            (2_000 * NAD, 100),
            (2_000 * NAD, 199_000_000),
        ] {
            for bridge in [BridgeStart::Quarter, BridgeStart::Half] {
                let model = FiniteC1::new(peak, scale, bridge).unwrap_or_else(|error| {
                    panic!("extreme profile failed: peak={peak} scale={scale} bridge={bridge:?} error={error:?}")
                });
                assert_eq!(model.q_for_reserves(1, u64::MAX).unwrap().1, Region::Tail);
                assert_eq!(model.q_for_reserves(u64::MAX, 1).unwrap().1, Region::Tail);
                assert!(model.invariant(1, u64::MAX).is_ok());
                let extreme_restore = model.quote_exact_in(1, u64::MAX, 1).unwrap();
                assert!(extreme_restore.used_exact_tail);
                assert!(extreme_restore.amount_out > 0);
                assert_eq!(extreme_restore.iterations.total(), 0);

                let reserve = u64::MAX / 4;
                let quote = model.quote_exact_in(reserve, reserve, 1_000_000).unwrap();
                assert!(quote.amount_out > 0);
                assert!(quote.iterations.invariant_iterations <= MAX_SOLVER_ITERATIONS as u32);
                assert!(quote.iterations.reserve_iterations <= MAX_SOLVER_ITERATIONS as u32);
            }
        }
    }

    #[test]
    fn deterministic_iteration_matrix_compares_both_bridge_candidates() {
        let summarize = |bridge| {
            let mut quotes = 0_u32;
            let mut exact_tail_quotes = 0_u32;
            let mut q_evaluations = 0_u64;
            let mut total_iterations = 0_u64;
            for (peak, scale) in PROFILES {
                let model = FiniteC1::new(peak, scale, bridge).unwrap();
                let geometry = model.geometry();
                let ratios = [
                    Q48,
                    (Q48 + geometry.reserve_ratio_start_q48) / 2,
                    (geometry.reserve_ratio_start_q48 + geometry.reserve_ratio_tail_q48) / 2,
                    geometry.reserve_ratio_tail_q48 + (geometry.reserve_ratio_tail_q48 - Q48),
                ];
                for ratio in ratios {
                    for high_first in [true, false] {
                        let (x, y) = state_for_ratio(ratio, high_first);
                        for divisor in [10_000_u64, 1_000, 100] {
                            let exact_in =
                                model.quote_exact_in(x, y, (x / divisor).max(1)).unwrap();
                            let exact_out =
                                model.quote_exact_out(x, y, (y / divisor).max(1)).unwrap();
                            for quote in [exact_in, exact_out] {
                                assert!(
                                    quote.iterations.inner_q_iterations
                                        <= quote.iterations.q_evaluations * 64
                                );
                                assert!(
                                    quote.iterations.invariant_iterations
                                        <= MAX_SOLVER_ITERATIONS as u32
                                );
                                assert!(
                                    quote.iterations.reserve_iterations
                                        <= MAX_SOLVER_ITERATIONS as u32
                                );
                                quotes += 1;
                                exact_tail_quotes += u32::from(quote.used_exact_tail);
                                q_evaluations += u64::from(quote.iterations.q_evaluations);
                                total_iterations += u64::from(quote.iterations.total());
                            }
                        }
                    }
                }
            }
            (quotes, q_evaluations, total_iterations, exact_tail_quotes)
        };

        let quarter = summarize(BridgeStart::Quarter);
        let half = summarize(BridgeStart::Half);
        assert_eq!(quarter, (144, 6_120, 183_488, 36));
        assert_eq!(half, (144, 6_122, 187_266, 36));
        assert!(quarter.2 < half.2);
    }

    #[test]
    fn reproduced_max_profile_inner_join_has_no_formula_jump() {
        let model = FiniteC1::new(2_000 * NAD, 199_000_000, BridgeStart::Quarter).unwrap();
        let x = 1_574_447_182_407_527_822_u64;
        let y = 1_000_000_000_000_000_000_u64;
        let crossing = quote_crossing(model, x, y, Region::Transition, true);
        let before = model.quote_exact_in(x, y, crossing.amount_in - 1).unwrap();
        let after = model.quote_exact_in(x, y, crossing.amount_in + 1).unwrap();
        assert_eq!(before.region_after, Region::Inner);
        assert_eq!(crossing.region_after, Region::Transition);
        assert!(before.amount_out <= crossing.amount_out);
        assert!(crossing.amount_out <= after.amount_out);
        assert!(crossing.amount_out - before.amount_out <= 2);
        assert!(after.amount_out - crossing.amount_out <= 2);
    }

    #[test]
    fn reproduced_max_profile_tail_join_has_no_formula_jump() {
        let model = FiniteC1::new(2_000 * NAD, 199_000_000, BridgeStart::Quarter).unwrap();
        let x = 8_785_111_725_555_100_480_u64;
        let y = 1_000_000_000_000_000_000_u64;
        let crossing = quote_crossing(model, x, y, Region::Tail, true);
        let before = (1..=4_u64).find_map(|offset| {
            crossing
                .amount_in
                .checked_sub(offset)
                .and_then(|input| model.quote_exact_in(x, y, input).ok())
        });
        let after = model.quote_exact_in(x, y, crossing.amount_in + 1).unwrap();
        assert_eq!(
            before.map_or(model.q_for_reserves(x, y).unwrap().1, |quote| quote
                .region_after),
            Region::Transition
        );
        assert_eq!(crossing.region_after, Region::Tail);
        let before_output = before.map_or(0, |quote| quote.amount_out);
        assert!(before_output <= crossing.amount_out);
        assert!(crossing.amount_out <= after.amount_out);
        assert!(crossing.amount_out - before_output <= 2);
        assert!(after.amount_out - crossing.amount_out <= 2);
    }

    #[test]
    fn q64_transition_quotes_have_no_stored_slack_or_adjacent_staircase() {
        let fixtures = [
            (
                100 * NAD,
                100_000_000,
                18_446_744_073_708_551_613_u64,
                5_671_181_913_068_248_712_u64,
                114_643_u64,
            ),
            (
                100 * NAD,
                100_000_000,
                999_999_999_999_999_999_u64,
                307_435_387_535_471_386_u64,
                12_429_u64,
            ),
            (
                2_000 * NAD,
                1_000_000,
                18_446_744_073_708_551_614_u64,
                16_603_345_133_192_447_584_u64,
                104_074_u64,
            ),
        ];
        for (peak, scale, x, y, adjacent_input) in fixtures {
            let model = FiniteC1::new(peak, scale, BridgeStart::Quarter).unwrap();
            let quote_or_zero = |amount_in| match model.quote_exact_in(x, y, amount_in) {
                Ok(quote) => quote.amount_out,
                Err(Error::UnsupportedState) => 0,
                Err(error) => panic!("unexpected quote failure: {error:?}"),
            };
            let one = quote_or_zero(1);
            let twenty = quote_or_zero(20);
            assert!(one <= 1, "one raw atom harvested {one} atoms");
            assert!(twenty <= 20, "twenty raw atoms harvested {twenty} atoms");

            let before = quote_or_zero(adjacent_input);
            let after = quote_or_zero(adjacent_input + 1);
            assert!(before <= after);
            assert!(
                after - before <= 1,
                "adjacent quote staircase: {before}->{after}"
            );

            let mut sequential_x = x;
            let mut sequential_y = y;
            let mut sequential_output = 0_u64;
            for _ in 0..20 {
                match model.quote_exact_in(sequential_x, sequential_y, 1) {
                    Ok(quote) => {
                        sequential_x = quote.reserve_in_after;
                        sequential_y = quote.reserve_out_after;
                        sequential_output += quote.amount_out;
                    }
                    Err(Error::UnsupportedState) => break,
                    Err(error) => panic!("unexpected sequential quote failure: {error:?}"),
                }
            }
            assert!(sequential_output <= twenty);
        }
    }
}
