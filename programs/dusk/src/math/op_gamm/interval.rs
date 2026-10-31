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

use crate::math::div_rem_native;

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
    #[inline(always)]
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
    let square = unsigned_ratio(z.lo as u128, z.lo as u128, ONE as u128, false)?;
    let mut term = z.lo as u128;
    let mut sum = 0_u128;
    for k in 0..24 {
        if term == 0 {
            break;
        }
        sum = sum
            .checked_add(((term as u64) / (2 * k + 1) as u64) as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        term = unsigned_ratio(term, square, ONE as u128, false)?;
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
    let mut term = ONE as u128;
    let mut sum = term;
    for k in 1..=24 {
        if term == 0 {
            break;
        }
        term = ((unsigned_ratio(term, reduced.lo as u128, ONE as u128, false)? as u64) / k as u64) as u128;
        sum = sum.checked_add(term).ok_or(ErrorCode::MarketMathOverflow)?;
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
#[derive(Clone, Copy, Debug)]
pub(super) struct Jet(pub [Interval; 9]);
#[cfg(test)]
impl Jet {
    #[inline(never)]
    pub fn constant(v: Interval) -> Self {
        {
            let mut out = [Interval::ZERO; 9];
            out[0] = v;
            Self(out)
        }
    }
    #[inline(never)]
    pub fn linear_exp(t: Interval, slope: Interval, offset: Interval) -> Result<Self> {
        let mut out = [Interval::ZERO; 9];
        out[0] = t.mul(slope)?.add(offset)?.exp()?;
        for i in 1..9 {
            out[i] = out[i - 1].mul(slope)?.divide_integer(i as i128)?;
        }
        Ok(Self(out))
    }
    #[inline(never)]
    pub fn add(self, b: Self) -> Result<Self> {
        let mut out = [Interval::ZERO; 9];
        for (i, v) in out.iter_mut().enumerate() {
            *v = self.0[i].add(b.0[i])?;
        }
        Ok(Self(out))
    }
    #[inline(never)]
    pub fn mul(self, b: Self) -> Result<Self> {
        let mut out = [Interval::ZERO; 9];
        for (i, v) in out.iter_mut().enumerate() {
            for j in 0..=i {
                *v = v.add(self.0[j].mul(b.0[i - j])?)?;
            }
        }
        Ok(Self(out))
    }
    #[inline(never)]
    pub fn div(self, b: Self) -> Result<Self> {
        let mut out = [Interval::ZERO; 9];
        for i in 0..9 {
            let mut v = self.0[i];
            for j in 1..=i {
                v = v.sub(b.0[j].mul(out[i - j])?)?;
            }
            out[i] = v.div(b.0[0])?;
        }
        Ok(Self(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_limb_division_matches_wide_division() {
        let mut seed = 0x8f17_c895_dad2_4c6a_3755_1f04_1122_0001_u128;
        for i in 0..100_000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let d = ((seed >> 32) as u64 >> (i % 63)).max(1) as u128;
            let n = if i % 2 == 0 {
                seed
            } else {
                (seed % d) << 64 | (seed as u64 as u128)
            };
            assert_eq!(div_rem_native(n, d), (n / d, n % d), "n={n} d={d}");
        }
        for d in [1, 2, 3, 1 << 31, 1 << 32, (1 << 32) + 1, 1 << 63, u64::MAX as u128] {
            for n in [
                0,
                1,
                d - 1,
                d,
                d + 1,
                u64::MAX as u128,
                (d - 1) << 64 | u64::MAX as u128,
                u128::MAX,
            ] {
                assert_eq!(div_rem_native(n, d), (n / d, n % d));
            }
        }
    }
    #[test]
    fn transcendental_intervals_enclose_decimal_reference_values() {
        // Q56 floors independently computed with 100-digit Decimal arithmetic.
        for (n, d, ln_floor) in [
            (1, 1, 0),
            (2, 1, LN2_LO),
            (1, 2, -LN2_LO - 1),
            (3, 1, 79_163_358_301_925_545),
            (10, 1, 165_918_741_868_749_488),
        ] {
            let v = Interval::ratio(n, d).unwrap().ln().unwrap();
            assert!(v.lo <= ln_floor && v.hi >= ln_floor, "{n}/{d}: {v:?} != {ln_floor}");
            let round = v.exp().unwrap();
            let exact = Interval::ratio(n, d).unwrap();
            assert!(round.lo <= exact.lo && round.hi >= exact.hi);
        }
    }
    #[test]
    fn interval_arithmetic_keeps_negative_rounding_direction() {
        let a = Interval::point(-ONE - 1);
        let b = Interval::ratio(1, 3).unwrap();
        let product = a.mul(b).unwrap();
        assert!(product.lo <= (-ONE - 1) / 3 && product.hi >= (-ONE - 1) / 3);
        assert!(Interval::UNIT.div(Interval { lo: -1, hi: 1 }).is_err());
        assert!(Interval::ZERO.ln().is_err());
        assert!(Interval::point(i128::MAX).add(Interval::UNIT).is_err());
    }
    #[test]
    fn transcendental_bounds_enclose_independent_high_precision_grid() {
        let rows =
            include_str!("../../../../../analysis/op-gamm-implementation-2026-10-30/transcendental-reference-q56.csv");
        for row in rows.lines().skip(1) {
            let v: Vec<i128> = row.split(',').map(|x| x.parse().unwrap()).collect();
            let log = Interval::point(v[0]).ln().unwrap();
            assert!(log.lo <= v[1] + 1 && log.hi >= v[1], "ln {row}: {log:?}");
            if v[2] >= 0 {
                let exp = Interval::point(v[0]).exp().unwrap();
                assert!(exp.lo <= v[2] + 1 && exp.hi >= v[2], "exp {row}: {exp:?}");
            }
        }
    }
    #[test]
    fn signed_interval_fast_paths_match_all_endpoint_extrema() {
        // Each optimized sign case must agree with the independent four-corner
        // definition, including zero, sign crossings and fractional Q56 bits.
        let values = [-7 * ONE - 17, -ONE - 1, -1, 0, 1, ONE / 3, ONE + 7, 11 * ONE];
        for &a in &values {
            for &b in &values {
                if a > b {
                    continue;
                }
                for &c in &values {
                    for &d in &values {
                        if c > d {
                            continue;
                        }
                        let x = Interval { lo: a, hi: b };
                        let y = Interval { lo: c, hi: d };
                        let mut product = Interval {
                            lo: i128::MAX,
                            hi: i128::MIN,
                        };
                        let mut quotient = product;
                        let mut quotient_overflow = false;
                        for n in [a, b] {
                            for m in [c, d] {
                                product.lo = product.lo.min(rounded_ratio(n, m, ONE, false).unwrap());
                                product.hi = product.hi.max(rounded_ratio(n, m, ONE, true).unwrap());
                                if c > 0 || d < 0 {
                                    match (rounded_ratio(n, ONE, m, false), rounded_ratio(n, ONE, m, true)) {
                                        (Ok(lo), Ok(hi)) => {
                                            quotient.lo = quotient.lo.min(lo);
                                            quotient.hi = quotient.hi.max(hi);
                                        }
                                        _ => quotient_overflow = true,
                                    }
                                }
                            }
                        }
                        assert_eq!(x.mul(y).unwrap(), product, "mul {x:?} {y:?}");
                        if (c > 0 || d < 0) && !quotient_overflow {
                            assert_eq!(x.div(y).unwrap(), quotient, "div {x:?} {y:?}");
                        } else {
                            assert!(x.div(y).is_err());
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn limb_multiplication_matches_independent_wide_integer_arithmetic() {
        let mut seed = 0x726f756e64696e675f63657274696679_u128;
        for k in 0..4096 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let a = seed >> (k % 65);
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let b = seed >> ((k / 65) % 65);
            let product = U256::from(a) * U256::from(b);
            for up in [false, true] {
                let divisor = b.max(1);
                let dividend = U256::from(a) << 56;
                let div = U256::from(divisor);
                let quotient = dividend / div;
                let expected = quotient + U256::from(u8::from(up && dividend % div != U256::zero()));
                let computed = unsigned_ratio(a, ONE as u128, divisor, up);
                if expected > U256::from(u128::MAX) {
                    assert!(computed.is_err());
                } else {
                    assert_eq!(computed.unwrap(), expected.as_u128());
                }

                let exact = (product >> 56) + U256::from(u8::from(up && product.low_u64() & ((1_u64 << 56) - 1) != 0));
                let actual = unsigned_ratio(a, b, ONE as u128, up);
                if exact > U256::from(u128::MAX) {
                    assert!(actual.is_err());
                } else {
                    assert_eq!(actual.unwrap(), exact.as_u128());
                }
            }
        }
    }
}

// Nonnegative outward product for derivative majorants.
#[cfg(test)]
pub(super) fn mul_up(a: i128, b: i128) -> Result<i128> {
    require!(a >= 0 && b >= 0, ErrorCode::MarketMathOverflow);
    if a == 0 || b == 0 {
        return Ok(0);
    }
    if a == ONE {
        return Ok(b);
    }
    if b == ONE {
        return Ok(a);
    }
    rounded_ratio(a, b, ONE, true)
}

#[cfg(test)]
mod square_root_enclosure_tests {
    use super::*;
    #[test]
    fn upper_tangent_encloses_random_and_extreme_square_roots() {
        let mut seed = 0x58f32beaa1d2e8ff_u64;
        let mut random = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for i in 0..20_000 {
            let value = (((random() as u128) << 64) | random() as u128) >> (1 + i % 127);
            let width = (random() as u128) >> (i % 64);
            let hi = value.saturating_add(width).min(i128::MAX as u128);
            let result = Interval {
                lo: value as i128,
                hi: hi as i128,
            }
            .sqrt()
            .unwrap();
            let lo_root = U256::from(result.lo as u128);
            let hi_root = U256::from(result.hi as u128);
            let nlo = U256::from(value) << 56;
            let nhi = U256::from(hi) << 56;
            assert!(lo_root * lo_root <= nlo);
            assert!((lo_root + U256::one()) * (lo_root + U256::one()) > nlo);
            assert!(hi_root * hi_root >= nhi);
        }
        for value in [0, 1, ONE, i128::MAX] {
            let result = Interval::point(value).sqrt().unwrap();
            assert_eq!(result.lo, sqrt_q56(value, false).unwrap());
            assert_eq!(result.hi, sqrt_q56(value, true).unwrap());
        }
    }
}
