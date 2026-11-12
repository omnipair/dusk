//! Outward-rounded Q56 intervals for the bounded OP invariant certificate.
//!
//! No floating-point value authorizes a trade. Transcendental evaluations
//! enclose their real value; uncertain arithmetic rejects the certificate.
use crate::errors::ErrorCode;
use anchor_lang::prelude::*;

pub(super) const ONE: i128 = 1_i128 << 56;
const LN2_LO: i128 = 49_946_518_145_322_873;
// Match the native curve's compatibility boundary: uint 0.9 expands legacy
// integer APIs and an expression-position semicolon into the caller's crate.
#[allow(unknown_lints)] // The SBF compiler predates Rust 1.99's renamed lint.
#[allow(
    clippy::assign_op_pattern,
    clippy::manual_div_ceil,
    deprecated,
    semicolon_in_expressions_from_non_local_macros
)]
mod wide {
    use uint::construct_uint;
    construct_uint! { pub struct U256(4); }
}
use wide::U256;

use crate::math::{arithmetic::widening_mul_u64, div_rem_native};

fn sqrt_q56(value: i128, up: bool) -> Result<i128> {
    require!(value >= 0, ErrorCode::InvalidArgument);
    if value == 0 {
        return Ok(0);
    }
    if let Some(n) = (value as u128).checked_mul(ONE as u128) {
        let mut x = 1_u128 << (128 - n.leading_zeros()).div_ceil(2);
        for _ in 0..16 {
            let y = (x + div_rem_native(n, x).0) / 2;
            if y >= x {
                return narrow(x + u128::from(up && x * x != n), false);
            }
            x = y;
        }
        return err!(ErrorCode::MarketMathOverflow);
    }
    let n = U256::from(value as u128) << 56;
    let shift = n.bits().saturating_sub(128).div_ceil(2) * 2;
    let top = (n >> shift).as_u128();
    let mut x = U256::from(crate::math::isqrt(top) + 1) << (shift / 2);
    for _ in 0..16 {
        let y = (x + n / x) >> 1;
        if y >= x {
            let root = x.as_u128() + u128::from(up && x * x != n);
            return narrow(root, false);
        }
        x = y;
    }
    err!(ErrorCode::MarketMathOverflow)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Interval {
    pub lo: i128,
    pub hi: i128,
}

#[inline(always)]
fn narrow(magnitude: u128, negative: bool) -> Result<i128> {
    let value = i128::try_from(magnitude).map_err(|_| ErrorCode::MarketMathOverflow)?;
    Ok(if negative { -value } else { value })
}

// Keep the rare wide path out of arithmetic call sites. The common Q56
// product fits u128; specializing its power-of-two denominator avoids both
// software division and enormous inlined SBF branch bodies.
#[inline(always)]
fn unsigned_ratio(a: u128, b: u128, d: u128, up: bool) -> Result<u128> {
    require!(d > 0, ErrorCode::DenominatorOverflow);
    let Some(product) = a.checked_mul(b) else {
        return unsigned_ratio_wide(a, b, d, up);
    };
    let (quotient, remainder) = if d == ONE as u128 {
        (product >> 56, product & (ONE as u128 - 1) != 0)
    } else {
        let (q, r) = div_rem_native(product, d);
        (q, r != 0)
    };
    quotient
        .checked_add(u128::from(up && remainder))
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

#[inline(never)]
fn unsigned_ratio_wide(a: u128, b: u128, d: u128, up: bool) -> Result<u128> {
    let product = U256::from(a) * U256::from(b);
    let divisor = U256::from(d);
    let quotient = if d == ONE as u128 {
        product >> 56
    } else if b == ONE as u128 {
        let inverse = u128::MAX / d;
        let seed = (U256::from(a) * U256::from(inverse)) >> 72;
        require!(seed <= U256::from(u128::MAX), ErrorCode::MarketMathOverflow);
        let residual = product - seed * divisor;
        let correction = if residual <= U256::from(u128::MAX) {
            U256::from(div_rem_native(residual.as_u128(), d).0)
        } else {
            residual / divisor
        };
        seed + correction
    } else {
        product / divisor
    };
    require!(quotient <= U256::from(u128::MAX), ErrorCode::MarketMathOverflow);
    let remainder = product != quotient * divisor;
    quotient
        .as_u128()
        .checked_add(u128::from(up && remainder))
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

#[inline(always)]
fn rounded_ratio(a: i128, b: i128, denominator: i128, up: bool) -> Result<i128> {
    require!(denominator != 0, ErrorCode::DenominatorOverflow);
    let negative = (a < 0) ^ (b < 0) ^ (denominator < 0);
    narrow(
        unsigned_ratio(
            a.unsigned_abs(),
            b.unsigned_abs(),
            denominator.unsigned_abs(),
            up ^ negative,
        )?,
        negative,
    )
}

impl Interval {
    pub const ZERO: Self = Self::point(0);
    pub const UNIT: Self = Self::point(ONE);
    pub const fn point(value: i128) -> Self {
        Self { lo: value, hi: value }
    }
    pub fn integer(value: i128) -> Result<Self> {
        Ok(Self::point(
            value.checked_mul(ONE).ok_or(ErrorCode::MarketMathOverflow)?,
        ))
    }
    pub fn ratio(n: u128, d: u128) -> Result<Self> {
        Ok(Self {
            lo: narrow(unsigned_ratio(n, ONE as u128, d, false)?, false)?,
            hi: narrow(unsigned_ratio(n, ONE as u128, d, true)?, false)?,
        })
    }
    pub fn add(self, other: Self) -> Result<Self> {
        Ok(Self {
            lo: self.lo.checked_add(other.lo).ok_or(ErrorCode::MarketMathOverflow)?,
            hi: self.hi.checked_add(other.hi).ok_or(ErrorCode::MarketMathOverflow)?,
        })
    }
    pub fn neg(self) -> Result<Self> {
        Ok(Self {
            lo: self.hi.checked_neg().ok_or(ErrorCode::MarketMathOverflow)?,
            hi: self.lo.checked_neg().ok_or(ErrorCode::MarketMathOverflow)?,
        })
    }
    pub fn sub(self, other: Self) -> Result<Self> {
        self.add(other.neg()?)
    }
    #[inline(never)]
    pub fn mul(self, other: Self) -> Result<Self> {
        if self == Self::ZERO || other == Self::ZERO {
            return Ok(Self::ZERO);
        }
        if self == Self::UNIT {
            return Ok(other);
        }
        if other == Self::UNIT {
            return Ok(self);
        }
        if self.lo >= 0 && other.lo >= 0 {
            return Ok(Self {
                lo: rounded_ratio(self.lo, other.lo, ONE, false)?,
                hi: rounded_ratio(self.hi, other.hi, ONE, true)?,
            });
        }
        if self.hi <= 0 && other.hi <= 0 {
            return Ok(Self {
                lo: rounded_ratio(self.hi, other.hi, ONE, false)?,
                hi: rounded_ratio(self.lo, other.lo, ONE, true)?,
            });
        }
        if self.hi <= 0 && other.lo >= 0 {
            return Ok(Self {
                lo: rounded_ratio(self.lo, other.hi, ONE, false)?,
                hi: rounded_ratio(self.hi, other.lo, ONE, true)?,
            });
        }
        if self.lo >= 0 && other.hi <= 0 {
            return Ok(Self {
                lo: rounded_ratio(self.hi, other.lo, ONE, false)?,
                hi: rounded_ratio(self.lo, other.hi, ONE, true)?,
            });
        }
        let mut lo = i128::MAX;
        let mut hi = i128::MIN;
        for a in [self.lo, self.hi] {
            for b in [other.lo, other.hi] {
                lo = lo.min(rounded_ratio(a, b, ONE, false)?);
                hi = hi.max(rounded_ratio(a, b, ONE, true)?);
            }
        }
        Ok(Self { lo, hi })
    }
    pub fn div(self, other: Self) -> Result<Self> {
        require!(other.lo > 0 || other.hi < 0, ErrorCode::DenominatorOverflow);
        if other.hi < 0 {
            return self.neg()?.div(other.neg()?);
        }
        if self.lo >= 0 {
            return Ok(Self {
                lo: rounded_ratio(self.lo, ONE, other.hi, false)?,
                hi: rounded_ratio(self.hi, ONE, other.lo, true)?,
            });
        }
        Ok(Self {
            lo: rounded_ratio(self.lo, ONE, other.lo, false)?,
            hi: rounded_ratio(self.hi, ONE, if self.hi <= 0 { other.hi } else { other.lo }, true)?,
        })
    }
    #[inline(always)]
    pub fn scale(self, n: i128) -> Result<Self> {
        let a = self.lo.checked_mul(n).ok_or(ErrorCode::MarketMathOverflow)?;
        let b = self.hi.checked_mul(n).ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(Self {
            lo: a.min(b),
            hi: a.max(b),
        })
    }
    pub fn divide_integer(self, n: i128) -> Result<Self> {
        require!(n > 0, ErrorCode::DenominatorOverflow);
        if self.lo >= i64::MIN as i128 && self.hi <= i64::MAX as i128 && n <= i64::MAX as i128 {
            let n = n as i64;
            let (lo, hi) = (self.lo as i64, self.hi as i64);
            return Ok(Self {
                lo: lo.div_euclid(n) as i128,
                hi: (hi.div_euclid(n) + i64::from(hi.rem_euclid(n) != 0)) as i128,
            });
        }
        Ok(Self {
            lo: self.lo.div_euclid(n),
            hi: self.hi.div_euclid(n) + i128::from(self.hi.rem_euclid(n) != 0),
        })
    }
    pub fn abs_max(self) -> Result<i128> {
        narrow(self.lo.unsigned_abs().max(self.hi.unsigned_abs()), false)
    }
    pub fn widen(self, radius: i128) -> Result<Self> {
        require!(radius >= 0, ErrorCode::InvalidArgument);
        Ok(Self {
            lo: self.lo.checked_sub(radius).ok_or(ErrorCode::MarketMathOverflow)?,
            hi: self.hi.checked_add(radius).ok_or(ErrorCode::MarketMathOverflow)?,
        })
    }
    pub fn ln(self) -> Result<Self> {
        require!(self.lo > 0, ErrorCode::InvalidArgument);
        let lower = ln_point(self.lo)?;
        if self.lo == self.hi {
            return Ok(lower);
        }
        let growth = Self::point(self.hi - self.lo).div(Self::point(self.lo))?.hi;
        let upper = lower.hi.checked_add(growth).ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(Self {
            lo: lower.lo,
            hi: upper,
        })
    }
    pub fn sqrt(self) -> Result<Self> {
        let lower = sqrt_q56(self.lo, false)?;
        let upper = if lower > 0 {
            // Concavity: sqrt(n) <= L + (n-L^2)/(2L) for L>0.
            // L is the exact lower integer root. A ceil division therefore
            // encloses the upper endpoint without a second Newton solve.
            let n = U256::from(self.hi as u128) << 56;
            let l = lower as u128;
            let residual = n - U256::from(l) * U256::from(l);
            if residual <= U256::from(u128::MAX) {
                let (q, r) = div_rem_native(residual.as_u128(), 2 * l);
                narrow(l + q + u128::from(r > 0), false)?
            } else {
                sqrt_q56(self.hi, true)?
            }
        } else {
            sqrt_q56(self.hi, true)?
        };
        Ok(Self { lo: lower, hi: upper })
    }
    pub fn exp(self) -> Result<Self> {
        let lower = exp_point(self.lo)?;
        if self.lo == self.hi {
            return Ok(lower);
        }
        // exp(d) <= 1/(1-d) for 0 <= d < 1. For narrow
        // endpoint uncertainty this avoids a duplicate transcendental call.
        let width = self.hi - self.lo;
        let upper = if width < ONE / 1000 {
            Self::point(lower.hi).div(Self::point(ONE - width))?.hi
        } else {
            exp_point(self.hi)?.hi
        };
        Ok(Self {
            lo: lower.lo,
            hi: upper,
        })
    }
}

fn ln_point(value: i128) -> Result<Interval> {
    require!(value > 0, ErrorCode::InvalidArgument);
    let power = 127 - value.leading_zeros() as i32 - 56;
    let normalized = if power >= 0 {
        Interval::point(value).divide_integer(1_i128 << power)?
    } else {
        Interval::point(value).scale(1_i128 << -power)?
    };
    let scale = Interval {
        lo: LN2_LO,
        hi: LN2_LO + 1,
    }
    .scale(i128::from(power))?;
    if normalized == Interval::UNIT {
        return Ok(scale);
    }
    let z = normalized.sub(Interval::UNIT)?.div(normalized.add(Interval::UNIT)?)?;
    require!(z.lo >= 0 && z.hi < ONE / 2, ErrorCode::MarketMathOverflow);
    // The ordered interval and range check give 0 <= z.lo < ONE / 2.
    // Both factors stay in u64 as each floor-rounded term decreases.
    let mut term = z.lo as u64;
    let square = (widening_mul_u64(term, term) >> 56) as u64;
    let mut sum = 0_u128;
    for k in 0..24 {
        if term == 0 {
            break;
        }
        sum = sum
            .checked_add(u128::from(term / (2 * k + 1) as u64))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        term = (widening_mul_u64(term, square) >> 56) as u64;
    }
    let lower = narrow(sum.checked_mul(2).ok_or(ErrorCode::MarketMathOverflow)?, false)?;
    // Here z<=1/3 plus a Q56 rounding unit. Floor-series error is
    // <64 units; the omitted tail is <1. The 192-unit allowance also
    // covers all term divisions. d[2 atanh(z)]/dz <3 covers z's width.
    let upper = lower
        .checked_add(192)
        .and_then(|v| v.checked_add(3 * (z.hi - z.lo)))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Interval { lo: lower, hi: upper }.add(scale)
}

fn exp_point(value: i128) -> Result<Interval> {
    if value == 0 {
        return Ok(Interval::UNIT);
    }
    if value < 0 {
        return Interval::UNIT.div(exp_point(value.checked_neg().ok_or(ErrorCode::MarketMathOverflow)?)?);
    }
    let power = value / (LN2_LO + 1);
    require!(power <= 60, ErrorCode::MarketMathOverflow);
    let reduced = Interval::point(value).sub(
        Interval {
            lo: LN2_LO,
            hi: LN2_LO + 1,
        }
        .scale(power)?,
    )?;
    require!(
        reduced.lo >= 0 && reduced.hi < 7 * ONE / 10,
        ErrorCode::MarketMathOverflow
    );
    // The ordered interval and range check give 0 <= reduced.lo < ONE.
    // Floor rounding and division by k keep every term in 0..=ONE.
    let reduced_lo = reduced.lo as u64;
    let mut term = ONE as u64;
    let mut sum = u128::from(term);
    for k in 1..=24 {
        if term == 0 {
            break;
        }
        term = ((widening_mul_u64(term, reduced_lo) >> 56) as u64) / k as u64;
        sum = sum.checked_add(u128::from(term)).ok_or(ErrorCode::MarketMathOverflow)?;
    }
    let lower = narrow(sum, false)?;
    // In [0,.7], each term's accumulated floor error is <2 Q56
    // units and the omitted tail is <1. exp'(r)<3 bounds range error.
    let upper = lower
        .checked_add(128)
        .and_then(|v| v.checked_add(3 * (reduced.hi - reduced.lo)))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Interval { lo: lower, hi: upper }.scale(1_i128 << power)
}

// Taylor coefficients f^(k)/k!, evaluated over the complete input interval.
// Interval recurrence independently checks the eighth-derivative majorant.

#[cfg(test)]
include!("../../tests/support/op_gamm_interval.rs");

#[cfg(test)]
mod square_root_enclosure_tests {
    include!("../../tests/math/op_gamm_interval_square_root.rs");
}
