//! Bounded oracle marginal pricing and an enclosing conditional invariant.
//!
//! P = N exp(clamp(lambda ln(O/N), -ln(M), ln(M))). The certificate
//! integrates this rule across concentration and clipping boundaries. Native
//! zero mode is dispatched by callers before constructing this context.
mod interval;
use crate::{
    constants::NAD,
    errors::ErrorCode,
    math::{mul_div_u128, sqrt_ratio_nad},
    transitions::amm::{ConcentratedCurveParameters, ConcentratedCurvePoint},
};
use anchor_lang::prelude::*;
#[cfg(test)]
use interval::Jet;
use interval::{Interval as I, ONE};

const MAX_CELLS: usize = 128;
const ERROR_Q56: i128 = ONE / 1_000_000_000;
// Stop refining an already certified output at one part in 10^10 of the
// output inventory (or the larger integration/atom uncertainty). This
// controls solver work; it never relaxes the nonnegative invariant test.
const OUTPUT_REFINEMENT_DENOMINATOR: u128 = 10_000_000_000;

#[derive(Clone, Copy, Debug)]
struct Layer {
    weight: I,
    lower: I,
    upper: I,
    log_lower: I,
    log_upper: I,
}

#[derive(Clone, Copy, Debug)]
struct Band {
    left: I,
    right: I,
    r_left: I,
    r_right: I,
    u_left: I,
    u_right: I,
    a: I,
    b: I,
    d: I,
}

#[derive(Clone, Debug)]
struct TaylorCell {
    left: i128,
    right: i128,
    q: [I; 13],
    order: usize,
    error_density: i128,
}

#[derive(Clone, Debug, Default)]
struct IntegrationScratch {
    points: Vec<i128>,
    pending: Vec<(i128, i128, u8)>,
}

impl TaylorCell {
    fn integral(&self, left: i128, right: i128) -> Result<I> {
        require!(
            left >= self.left && right <= self.right && right >= left,
            ErrorCode::InvalidArgument
        );
        let half = I::point(self.right - self.left).divide_integer(2)?;
        let center = I::point(self.left).add(I::point(self.right))?.divide_integer(2)?;
        let v0 = I::point(left).sub(center)?.div(half)?;
        let v1 = I::point(right).sub(center)?.div(half)?;
        let (mut p0, mut p1) = (v0, v1);
        let mut total = I::ZERO;
        for k in 0..=self.order {
            total = total.add(self.q[k].mul(p1.sub(p0)?)?.divide_integer(k as i128 + 1)?)?;
            if k < self.order {
                p0 = p0.mul(v0)?;
                p1 = p1.mul(v1)?;
            }
        }
        let error = I::point(self.error_density).mul(I::point(right - left))?.hi;
        total.mul(half)?.widen(error)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BoundedOracleCurve {
    center: u64,
    parameters: ConcentratedCurveParameters,
    book_geometry: std::cell::RefCell<
        Vec<(
            ConcentratedCurvePoint,
            crate::state::ConcentratedCurveCache,
            crate::transitions::amm::ConcentratedCurveGeometry,
        )>,
    >,
    lambda: I,
    log_reference: I,
    log_cap: I,
    layers: Vec<Layer>,
    bands: Vec<Band>,
    breaks: Vec<I>,
    max_h: I,
    coordinates: std::cell::RefCell<Vec<(ConcentratedCurvePoint, I)>>,
    prices: std::cell::RefCell<Vec<(I, I)>>,
    primitives: std::cell::RefCell<Vec<(I, I)>>,
    integrals: std::cell::RefCell<Vec<TaylorCell>>,
    integration_scratch: std::cell::RefCell<IntegrationScratch>,
}

impl BoundedOracleCurve {
    pub(crate) fn new(
        parameters: ConcentratedCurveParameters,
        center: u64,
        reference: u64,
        lambda_bps: u16,
        cap_nad: u64,
    ) -> Result<Self> {
        require!(
            center > 0 && reference > 0 && lambda_bps > 0 && lambda_bps < 10_000 && cap_nad > NAD,
            ErrorCode::InvalidMarketConfig
        );
        parameters.validate(crate::state::MAX_AMM_AMPLIFICATION_NAD)?;
        let lambda = I::ratio(u128::from(lambda_bps), 10_000)?;
        let log_reference = I::ratio(reference as u128, center as u128)?.ln()?;
        let log_cap = I::ratio(cap_nad as u128, NAD as u128)?.ln()?;
        let mut layers = Vec::with_capacity(2);
        let mut breaks = Vec::with_capacity(6);
        let mut max_h = I::integer(2)?;
        if !parameters.is_cpmm() {
            let (tail, concentrated) = parameters.liquidity_shares_nad()?;
            let count = if parameters.fade_width_bps == 0 { 1 } else { 2 };
            let weight = I::ratio(concentrated, tail)?.divide_integer(count)?;
            for index in 0..count {
                let width = u128::from(parameters.core_half_width_bps)
                    + if index == 1 {
                        u128::from(parameters.fade_width_bps)
                    } else {
                        0
                    };
                let high = sqrt_ratio_nad(NAD as u128 + width * NAD as u128 / 10_000)?;
                let upper = I::ratio(high, NAD as u128)?;
                let lower = I::UNIT.div(upper)?;
                let log_lower = lower.ln()?;
                let log_upper = upper.ln()?;
                layers.push(Layer {
                    weight,
                    lower,
                    upper,
                    log_lower,
                    log_upper,
                });
                breaks.extend([log_lower, log_upper]);
                max_h = max_h.add(weight.scale(2)?)?;
            }
        }
        let reach = log_cap.div(lambda)?;
        breaks.extend([
            log_reference.sub(reach)?.divide_integer(2)?,
            log_reference.add(reach)?.divide_integer(2)?,
        ]);
        let mut curve = Self {
            center,
            parameters,
            book_geometry: Default::default(),
            lambda,
            log_reference,
            log_cap,
            layers,
            bands: Vec::with_capacity(5),
            breaks,
            max_h,
            coordinates: std::cell::RefCell::new(Vec::with_capacity(16)),
            prices: std::cell::RefCell::new(Vec::with_capacity(16)),
            primitives: Default::default(),
            integrals: Default::default(),
            integration_scratch: Default::default(),
        };
        curve.build_bands()?;
        Ok(curve)
    }

    /// Pure native geometry for a book point. Shape and center are immutable
    /// in this authenticated curve; the point is the only varying cache key.
    pub(crate) fn native_geometry_at(
        &self,
        point: ConcentratedCurvePoint,
    ) -> Result<(
        crate::state::ConcentratedCurveCache,
        crate::transitions::amm::ConcentratedCurveGeometry,
    )> {
        if let Some((_, cache, geometry)) = self.book_geometry.borrow().iter().find(|(key, _, _)| *key == point) {
            return Ok((*cache, *geometry));
        }
        let value = crate::transitions::amm::curve::prepare_concentrated_cache_and_geometry_at_point(
            point.base_reserve,
            point.quote_reserve,
            self.center,
            self.parameters,
        )?;
        let mut memo = self.book_geometry.borrow_mut();
        if let Some(entry) = memo.first_mut() {
            *entry = (point, value.0, value.1);
        } else {
            memo.reserve_exact(1);
            memo.push((point, value.0, value.1));
        }
        Ok(value)
    }

    // X=A/u+B, Y=A*u+D on each native band. All coefficients share
    // a fixed, inventory-independent normalization; an OP endpoint does not
    // redefine the shape merely because it left the previous native curve.
    fn raw_coefficients(&self, t: i128) -> Result<(I, I, I)> {
        let (mut a, mut b, mut d) = (I::UNIT, I::ZERO, I::ZERO);
        for layer in &self.layers {
            if t < layer.log_lower.lo {
                b = b.add(
                    layer
                        .weight
                        .mul(I::UNIT.div(layer.lower)?.sub(I::UNIT.div(layer.upper)?)?)?,
                )?;
            } else if t > layer.log_upper.hi {
                d = d.add(layer.weight.mul(layer.upper.sub(layer.lower)?)?)?;
            } else {
                a = a.add(layer.weight)?;
                b = b.sub(layer.weight.div(layer.upper)?)?;
                d = d.sub(layer.weight.mul(layer.lower)?)?;
            }
        }
        Ok((a, b, d))
    }

    fn build_bands(&mut self) -> Result<()> {
        if self.layers.is_empty() {
            return Ok(());
        }
        let mut cuts = vec![(I::integer(-32)?, I::ZERO)];
        for layer in &self.layers {
            cuts.extend([(layer.log_lower, layer.lower), (layer.log_upper, layer.upper)]);
        }
        cuts.push((I::integer(32)?, I::ZERO));
        cuts.sort_by_key(|v| v.0.lo);
        for (i, pair) in cuts.windows(2).enumerate() {
            let middle = pair[0].0.hi + (pair[1].0.lo - pair[0].0.hi) / 2;
            let (a, b, d) = self.raw_coefficients(middle)?;
            let ratio = |u: I| -> Result<I> { a.mul(u)?.add(d)?.div(a.div(u)?.add(b)?) };
            let r_left = if i == 0 { I::ZERO } else { ratio(pair[0].1)? };
            let r_right = if i == cuts.len() - 2 {
                I::point(i128::MAX)
            } else {
                ratio(pair[1].1)?
            };
            self.bands.push(Band {
                left: pair[0].0,
                right: pair[1].0,
                r_left,
                r_right,
                u_left: pair[0].1,
                u_right: if i == cuts.len() - 2 {
                    I::point(i128::MAX)
                } else {
                    pair[1].1
                },
                a,
                b,
                d,
            });
        }
        Ok(())
    }

    fn coefficients(&self, t: i128) -> Result<(I, I, I)> {
        if self.bands.is_empty() {
            return Ok((I::UNIT, I::ZERO, I::ZERO));
        }
        let band = self
            .bands
            .iter()
            .find(|v| t >= v.left.lo && t <= v.right.hi)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok((band.a, band.b, band.d))
    }

    fn coordinate(&self, point: ConcentratedCurvePoint) -> Result<I> {
        if let Some((_, value)) = self.coordinates.borrow().iter().rev().find(|(key, _)| *key == point) {
            return Ok(*value);
        }
        let value = self.coordinate_uncached(point)?;
        let mut cache = self.coordinates.borrow_mut();
        if cache.len() >= 16 {
            cache.clear();
        }
        cache.push((point, value));
        Ok(value)
    }

    fn coordinate_uncached(&self, point: ConcentratedCurvePoint) -> Result<I> {
        require!(
            point.base_reserve > 0 && point.quote_reserve > 0,
            ErrorCode::InsufficientLiquidity
        );
        let r = I::ratio(point.quote_reserve, point.base_reserve)?.mul(I::ratio(NAD as u128, self.center as u128)?)?;
        if self.layers.is_empty() {
            return r.ln()?.divide_integer(2);
        }
        let mut low = i128::MAX;
        let mut high = i128::MIN;
        for band in &self.bands {
            if r.hi < band.r_left.lo || r.lo > band.r_right.hi {
                continue;
            }
            let (a, b, d) = (band.a, band.b, band.d);
            let v = d.sub(r.mul(b)?)?;
            let root = v.mul(v)?.add(r.mul(a.mul(a)?)?.scale(4)?)?.sqrt()?;
            let u = if v.lo > 0 {
                r.mul(a)?.scale(2)?.div(v.add(root)?)?
            } else {
                root.sub(v)?.div(a.scale(2)?)?
            };
            if u.lo <= 0 {
                continue;
            }
            let t = u.ln()?;
            if t.hi >= band.left.lo && t.lo <= band.right.hi {
                low = low.min(t.lo.max(band.left.lo));
                high = high.max(t.hi.min(band.right.hi));
            }
        }
        require!(low <= high, ErrorCode::MarketMathOverflow);
        Ok(I { lo: low, hi: high })
    }

    fn normalized_price(&self, t: I, log_reference: I) -> Result<I> {
        let native = t.scale(2)?;
        let correction = log_reference.sub(native)?.mul(self.lambda)?;
        let clipped = I {
            lo: correction.lo.clamp(-self.log_cap.hi, self.log_cap.lo),
            hi: correction.hi.clamp(-self.log_cap.lo, self.log_cap.hi),
        };
        native.add(clipped)?.exp()
    }
    fn price_at_coordinate(&self, t: I) -> Result<I> {
        if let Some((_, value)) = self.prices.borrow().iter().rev().find(|(key, _)| *key == t) {
            return Ok(*value);
        }
        let value = self
            .normalized_price(t, self.log_reference)?
            .mul(I::ratio(self.center as u128, NAD as u128)?)?;
        let mut cache = self.prices.borrow_mut();
        if cache.len() >= 16 {
            cache.clear();
        }
        cache.push((t, value));
        Ok(value)
    }

    pub(crate) fn price_bounds_nad(&self, point: ConcentratedCurvePoint) -> Result<(u64, u64)> {
        let p = self.price_at_coordinate(self.coordinate(point)?)?;
        require!(p.lo > 0, ErrorCode::InvalidSettlementPrice);
        let lo = mul_div_u128(p.lo as u128, NAD as u128, ONE as u128)?;
        let hi = crate::math::mul_div_ceil_u128(p.hi as u128, NAD as u128, ONE as u128)?;
        Ok((
            u64::try_from(lo).map_err(|_| ErrorCode::MarketMathOverflow)?,
            u64::try_from(hi).map_err(|_| ErrorCode::MarketMathOverflow)?,
        ))
    }

    pub(crate) fn spot_price_nad(&self, point: ConcentratedCurvePoint) -> Result<u64> {
        let (lower, upper) = self.price_bounds_nad(point)?;
        Ok(lower + (upper - lower) / 2)
    }

    /// Historical reference is used only for volatility attribution. The
    /// authenticated curve and every executable quote retain their bound source.
    pub(crate) fn historical_mark_nad(&self, point: ConcentratedCurvePoint, reference: u64) -> Result<u64> {
        let log = I::ratio(reference as u128, self.center as u128)?.ln()?;
        let price = self
            .normalized_price(self.coordinate(point)?, log)?
            .mul(I::ratio(self.center as u128, NAD as u128)?)?;
        let middle = price.lo + (price.hi - price.lo) / 2;
        u64::try_from(mul_div_u128(middle as u128, NAD as u128, ONE as u128)?)
            .map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    fn exponent(&self, region: i128) -> Result<(I, I)> {
        let correction = self.log_reference.sub(I::point(region).scale(2)?)?.mul(self.lambda)?;
        if correction.lo > self.log_cap.hi {
            Ok((I::integer(2)?, self.log_cap))
        } else if correction.hi < -self.log_cap.hi {
            Ok((I::integer(2)?, self.log_cap.neg()?))
        } else {
            Ok((
                I::UNIT.sub(self.lambda)?.scale(2)?,
                self.log_reference.mul(self.lambda)?,
            ))
        }
    }

    #[cfg(test)]
    #[inline(never)]
    fn integrand(&self, t: I, region: i128) -> Result<Jet> {
        let (a, b, d) = self.coefficients(region)?;
        let (slope, offset) = self.exponent(region)?;
        let u = Jet::linear_exp(t, I::UNIT, I::ZERO)?;
        let v = Jet::linear_exp(t, slope.sub(I::UNIT)?, offset)?;
        let p = Jet::linear_exp(t, slope, offset)?;
        residual_derivative_fraction(a, b, d, &u, &v, &p)
    }

    // Integrate only the deviation from the native invariant. Since
    // -d(log X)/dt = A/(u X), subtracting the native term cancels X:
    // d(G-G_native)/dt = A (u-p/u)/(Y+pX).
    // Scaled Taylor majorants for N/Z. For positive Z, reciprocal
    // coefficients follow |(1/Z)_k| <= sum |Z_i| |(1/Z)_(k-i)| / Z_min.
    // Each coefficient includes cell_width^k to avoid intermediate overflow.
    #[cfg(test)]
    #[inline(never)]
    fn derivative_bound(&self, t: I, region: i128, order: usize) -> Result<i128> {
        let width = I::point(t.hi - t.lo);
        let (a, b, d) = self.coefficients(region)?;
        let (slope, offset) = self.exponent(region)?;
        let u = t.exp()?;
        let inverse = I::UNIT.div(u)?;
        let x = a.mul(inverse)?.add(b)?;
        let y = a.mul(u)?.add(d)?;
        let p = t.mul(slope)?.add(offset)?.exp()?;
        let z = y.add(p.mul(x)?)?;
        require!(z.lo > 0, ErrorCode::MarketMathOverflow);
        let mut zd = [0_i128; 9];
        let mut nd = [0_i128; 9];
        let mut au = a.mul(u)?;
        let mut av = a.mul(p)?.mul(inverse)?;
        let mut bp = b.mul(p)?;
        zd[0] = z.lo;
        nd[0] = au.sub(av)?.abs_max()?;
        let beta = slope.mul(width)?;
        let beta_minus_one = slope.sub(I::UNIT)?.mul(width)?;
        for k in 1..=order {
            au = au.mul(width)?.divide_integer(k as i128)?;
            av = av.mul(beta_minus_one)?.divide_integer(k as i128)?;
            bp = bp.mul(beta)?.divide_integer(k as i128)?;
            zd[k] = au.add(av)?.add(bp)?.abs_max()?;
            nd[k] = au.sub(av)?.abs_max()?;
        }
        let reciprocal = reciprocal_majorant(&zd, order)?;
        let mut bound = 0;
        for k in 0..=order {
            bound = checked_sum(bound, interval::mul_up(nd[k], reciprocal[order - k])?)?;
        }
        Ok(bound)
    }

    fn native_primitive(&self, t: I) -> Result<I> {
        if let Some((_, value)) = self.primitives.borrow().iter().rev().find(|(key, _)| *key == t) {
            return Ok(*value);
        }
        let value = self.native_primitive_uncached(t)?;
        let mut cache = self.primitives.borrow_mut();
        if cache.len() >= 16 {
            cache.clear();
        }
        cache.push((t, value));
        Ok(value)
    }

    fn native_primitive_uncached(&self, t: I) -> Result<I> {
        if self.layers.is_empty() {
            return Ok(t);
        }
        let inverse = t.neg()?.exp()?;
        let mut low = i128::MAX;
        let mut high = i128::MIN;
        for band in &self.bands {
            if t.hi < band.left.lo || t.lo > band.right.hi {
                continue;
            }
            let x = band.a.mul(inverse)?.add(band.b)?;
            let value = x.ln()?.neg()?;
            low = low.min(value.lo);
            high = high.max(value.hi);
        }
        require!(low <= high, ErrorCode::MarketMathOverflow);
        Ok(I { lo: low, hi: high })
    }

    // Exact elementary primitives where the normalized price is k*u or
    // k*u^2. Other weights retain the same certified numerical path.
    fn analytic_residual(&self, left: i128, right: i128) -> Result<Option<I>> {
        if self.layers.is_empty() {
            return self.cpmm_integral(left, right, left + (right - left) / 2).map(Some);
        }
        let mid = left + (right - left) / 2;
        let (a, b, d) = self.coefficients(mid)?;
        let (slope, offset) = self.exponent(mid)?;
        if slope != I::UNIT && slope != I::integer(2)? {
            return Ok(None);
        }
        let k = offset.exp()?;
        let u0 = I::point(left).exp()?;
        let u1 = I::point(right).exp()?;
        let width = I::point(right - left);
        if slope == I::UNIT {
            let l = a.add(k.mul(b)?)?;
            let h = d.add(k.mul(a)?)?;
            if l.lo <= 0 && l.hi >= 0 || h.lo <= 0 && h.hi >= 0 {
                return Ok(None);
            }
            let z0 = l.mul(u0)?.add(h)?;
            let z1 = l.mul(u1)?.add(h)?;
            let logarithm = z1.div(z0)?.ln()?;
            let kh = k.div(h)?;
            let result = a.mul(I::UNIT.div(l)?.add(kh)?.mul(logarithm)?.sub(kh.mul(width)?)?)?;
            return Ok(Some(result));
        }
        // Integral A(1-k) du / [k B u^2 + A(1+k)u + D].
        let qa = k.mul(b)?;
        let qb = a.mul(I::UNIT.add(k)?)?;
        let qc = d;
        let scale = a.mul(I::UNIT.sub(k)?)?;
        let primitive_delta = if qa == I::ZERO {
            qb.mul(u1)?.add(qc)?.div(qb.mul(u0)?.add(qc)?)?.ln()?.div(qb)?
        } else if qc == I::ZERO {
            width
                .sub(qa.mul(u1)?.add(qb)?.div(qa.mul(u0)?.add(qb)?)?.ln()?)?
                .div(qb)?
        } else {
            let root = qb.mul(qb)?.sub(qa.mul(qc)?.scale(4)?)?.sqrt()?;
            if root.lo <= 0 {
                return Ok(None);
            }
            let abs = |v: I| -> Result<I> {
                if v.lo > 0 {
                    Ok(v)
                } else if v.hi < 0 {
                    v.neg()
                } else {
                    err!(ErrorCode::MarketMathOverflow)
                }
            };
            let log_ratio = |u: I| -> Result<I> {
                let w = qa.mul(u)?.scale(2)?.add(qb)?;
                abs(w.sub(root)?)?.div(abs(w.add(root)?)?)?.ln()
            };
            log_ratio(u1)?.sub(log_ratio(u0)?)?.div(root)?
        };
        Ok(Some(scale.mul(primitive_delta)?))
    }

    fn cpmm_integral(&self, a: i128, b: i128, mid: i128) -> Result<I> {
        let correction = self.log_reference.sub(I::point(mid).scale(2)?)?.mul(self.lambda)?;
        if correction.lo > self.log_cap.hi || correction.hi < -self.log_cap.hi {
            let cap = if correction.lo > self.log_cap.hi {
                self.log_cap
            } else {
                self.log_cap.neg()?
            };
            return I::point(b - a)
                .scale(2)?
                .div(I::UNIT.add(cap.exp()?)?)?
                .sub(I::point(b - a));
        }
        let reference = self.log_reference.mul(self.lambda)?.exp()?;
        let primitive = |t: i128| -> Result<I> {
            I::point(t)
                .scale(2)?
                .mul(self.lambda)?
                .exp()?
                .add(reference)?
                .ln()?
                .div(self.lambda)
        };
        primitive(b)?.sub(primitive(a)?)?.sub(I::point(b - a))
    }

    // A Taylor model of N/Z on v=(t-mid)/half in [-1,1]. Unlike a
    // derivative bound over the whole cell, coefficients are evaluated at
    // its midpoint. The remainder is certified from N-Q*Z, not inferred
    // from agreement between two numerical estimates.
    #[inline(never)]
    fn taylor_cell(&self, left: i128, right: i128, order: usize) -> Result<(I, TaylorCell)> {
        let mid = left + (right - left) / 2;
        let center = I::point(left).add(I::point(right))?.divide_integer(2)?;
        let half = I::point(right - left).divide_integer(2)?;
        let (a, b, d) = self.coefficients(mid)?;
        let (slope, offset) = self.exponent(mid)?;
        let alpha = slope.sub(I::UNIT)?;
        let mut au = a.mul(center.exp()?)?;
        let mut av = a.mul(center.mul(alpha)?.add(offset)?.exp()?)?;
        let mut bp = b.mul(center.mul(slope)?.add(offset)?.exp()?)?;
        let su = half;
        let sv = alpha.mul(half)?;
        let sp = slope.mul(half)?;
        let mut numerator = [I::ZERO; 13];
        let mut denominator = [I::ZERO; 13];
        for k in 0..=order {
            numerator[k] = au.sub(av)?;
            denominator[k] = au.add(av)?.add(bp)?;
            if k == 0 {
                denominator[k] = denominator[k].add(d)?;
            }
            if k < order {
                au = au.mul(su)?.divide_integer(k as i128 + 1)?;
                av = av.mul(sv)?.divide_integer(k as i128 + 1)?;
                bp = bp.mul(sp)?.divide_integer(k as i128 + 1)?;
            }
        }
        let tail = |term: I, scale: I| -> Result<I> {
            let scale = I::point(scale.abs_max()?);
            let denominator = I::UNIT.sub(scale.divide_integer(order as i128 + 2)?)?;
            require!(denominator.lo > 0, ErrorCode::MarketMathOverflow);
            I::point(term.abs_max()?)
                .mul(scale)?
                .divide_integer(order as i128 + 1)?
                .div(denominator)
        };
        let ntail = tail(au, su)?.add(tail(av, sv)?)?;
        let ztail = ntail.add(tail(bp, sp)?)?;
        let (integral, q, error_density) =
            taylor_quotient_integral(&numerator, &denominator, ntail, ztail, half, order)?;
        Ok((
            integral,
            TaylorCell {
                left,
                right,
                q,
                order,
                error_density,
            },
        ))
    }

    fn integral(&self, start: I, end: I) -> Result<I> {
        let mut cache = self.integrals.borrow_mut();
        if cache.len() >= MAX_CELLS {
            cache.clear();
        }
        self.integral_cached(start, end, &mut cache)
    }

    fn integral_cached(&self, start: I, end: I, cache: &mut Vec<TaylorCell>) -> Result<I> {
        let a = start.lo + (start.hi - start.lo) / 2;
        let b = end.lo + (end.hi - end.lo) / 2;
        if a > b {
            return self.integral_cached(end, start, cache)?.neg();
        }
        // Solana's default allocator does not reclaim dropped vectors. Reuse
        // the subdivision workspace across certificates in this invocation.
        let mut scratch = self.integration_scratch.borrow_mut();
        let IntegrationScratch { points, pending } = &mut *scratch;
        points.clear();
        pending.clear();
        points.extend([a, b]);
        for cut in &self.breaks {
            for p in [cut.lo, cut.hi] {
                if p > a && p < b {
                    points.push(p);
                }
            }
        }
        points.sort_unstable();
        points.dedup();
        for pair in points.windows(2) {
            pending.push((pair[0], pair[1], 0_u8));
        }
        let error_per_segment = ERROR_Q56 / (pending.len().max(1) as i128);
        let mut total = I::ZERO;
        let mut cells = 0;
        while let Some((left, right, depth)) = pending.pop() {
            cells += 1;
            require!(cells <= MAX_CELLS && depth <= 24, ErrorCode::MarketMathOverflow);
            let ambiguous = self.breaks.iter().any(|v| left >= v.lo && right <= v.hi);
            if ambiguous {
                total = total.add(
                    I {
                        lo: -self.max_h.hi,
                        hi: self.max_h.hi,
                    }
                    .mul(I::point(right - left))?,
                )?;
                continue;
            }
            let tolerance = error_per_segment / (1_i128 << depth);
            if let Some(cell) = cache.iter().find(|v| left >= v.left && right <= v.right) {
                let value = cell.integral(left, right)?;
                if value.hi - value.lo <= tolerance.max(64) {
                    total = total.add(value)?;
                    continue;
                }
            }
            if let Ok(Some(v)) = self.analytic_residual(left, right) {
                if v.hi - v.lo <= tolerance.max(64) {
                    total = total.add(v)?;
                    continue;
                }
            }
            let width = right - left;
            let order = if width < ONE / 100 {
                4
            } else if width < ONE / 10 {
                6
            } else if width < ONE {
                10
            } else {
                12
            };
            let result = self.taylor_cell(left, right, order);
            if let Ok((v, cell)) = result {
                if v.hi - v.lo <= tolerance.max(64) {
                    cache.push(cell);
                    total = total.add(v)?;
                    continue;
                }
            }
            let mid = left + (right - left) / 2;
            require!(mid > left && mid < right, ErrorCode::MarketMathOverflow);
            pending.push((mid, right, depth + 1));
            pending.push((left, mid, depth + 1));
        }
        let endpoint_error = self.max_h.mul(I::point(start.hi - start.lo + end.hi - end.lo))?.hi;
        total.widen(endpoint_error)
    }

    fn elasticity(&self, t: I) -> Result<I> {
        let mid = t.lo + (t.hi - t.lo) / 2;
        let (a, b, d) = self.coefficients(mid)?;
        let u = t.exp()?;
        let x = a.div(u)?.add(b)?;
        let y = a.mul(u)?.add(d)?;
        let log_ratio_derivative = a.mul(u)?.div(y)?.add(a.div(u.mul(x)?)?)?;
        self.exponent(mid)?.0.div(log_ratio_derivative)
    }

    #[inline(never)]
    fn native_seed(&self, start: ConcentratedCurvePoint, t: I, input: u128, base_to_quote: bool) -> Result<u128> {
        if self.layers.is_empty() {
            let (reserve_in, reserve_out) = if base_to_quote {
                (start.base_reserve, start.quote_reserve)
            } else {
                (start.quote_reserve, start.base_reserve)
            };
            return mul_div_u128(
                input,
                reserve_out,
                reserve_in.checked_add(input).ok_or(ErrorCode::MarketMathOverflow)?,
            );
        }
        let u = t.exp()?;
        let (a, b, d) = self.coefficients(t.lo + (t.hi - t.lo) / 2)?;
        let x0 = a.div(u)?.add(b)?;
        let y0 = a.mul(u)?.add(d)?;
        let ratio = I::UNIT.add(I::ratio(
            input,
            if base_to_quote {
                start.base_reserve
            } else {
                start.quote_reserve
            },
        )?)?;
        let target = if base_to_quote { x0 } else { y0 }.mul(ratio)?;
        for band in &self.bands {
            let u = if base_to_quote {
                let denominator = target.sub(band.b)?;
                if denominator.lo <= 0 {
                    continue;
                }
                band.a.div(denominator)?
            } else {
                let numerator = target.sub(band.d)?;
                if numerator.lo <= 0 {
                    continue;
                }
                numerator.div(band.a)?
            };
            if u.hi < band.u_left.lo || u.lo > band.u_right.hi {
                continue;
            }
            let fraction = if base_to_quote {
                band.a.mul(u)?.add(band.d)?.div(y0)?
            } else {
                band.a.div(u)?.add(band.b)?.div(x0)?
            };
            let output_fraction = I::UNIT.sub(fraction)?.lo.max(0) as u128;
            return mul_div_u128(
                if base_to_quote {
                    start.quote_reserve
                } else {
                    start.base_reserve
                },
                output_fraction,
                ONE as u128,
            );
        }
        err!(ErrorCode::MarketMathOverflow)
    }

    /// Solve an exact-input trade in normalized inventory coordinates. Every
    /// accepted output has an enclosing nonnegative invariant certificate.
    /// The Newton step selects candidates only; it never authorizes output.
    #[cfg(any(test, feature = "benchmark"))]
    pub(crate) fn quote_exact_in(
        &self,
        start: ConcentratedCurvePoint,
        input: u128,
        output_quantum: u128,
        base_to_quote: bool,
    ) -> Result<(u128, ConcentratedCurvePoint)> {
        let inventory = if base_to_quote {
            start.quote_reserve
        } else {
            start.base_reserve
        };
        self.solve_output(start, input, output_quantum, base_to_quote, inventory, 0, |output| {
            Ok(if base_to_quote {
                ConcentratedCurvePoint {
                    base_reserve: start
                        .base_reserve
                        .checked_add(input)
                        .ok_or(ErrorCode::ReserveOverflow)?,
                    quote_reserve: start
                        .quote_reserve
                        .checked_sub(output)
                        .ok_or(ErrorCode::InsufficientLiquidity)?,
                }
            } else {
                ConcentratedCurvePoint {
                    base_reserve: start
                        .base_reserve
                        .checked_sub(output)
                        .ok_or(ErrorCode::InsufficientLiquidity)?,
                    quote_reserve: start
                        .quote_reserve
                        .checked_add(input)
                        .ok_or(ErrorCode::ReserveOverflow)?,
                }
            })
        })
        .map(|(output, end, _)| (output, end))
    }

    /// The endpoint callback materializes ownership and integer debt before
    /// certification. A numerical root guess cannot authorize settlement.
    #[inline(never)]
    pub(crate) fn solve_output<F>(
        &self,
        start: ConcentratedCurvePoint,
        input: u128,
        output_quantum: u128,
        base_to_quote: bool,
        output_inventory: u128,
        required_delta: i128,
        mut endpoint: F,
    ) -> Result<(u128, ConcentratedCurvePoint, i128)>
    where
        F: FnMut(u128) -> Result<ConcentratedCurvePoint>,
    {
        require!(input > 0 && output_quantum > 0, ErrorCode::AmountZero);
        require!(required_delta >= 0, ErrorCode::InvalidArgument);
        let mut high = output_inventory
            .checked_sub(1)
            .ok_or(ErrorCode::InsufficientLiquidity)?
            / output_quantum;
        require!(high > 0, ErrorCode::InsufficientLiquidity);
        let mut low = 0_u128;
        let mut accepted = None;
        let start_t = self.coordinate(start)?;
        let price = self.price_at_coordinate(start_t)?;
        let native_price = start_t
            .scale(2)?
            .exp()?
            .mul(I::ratio(self.center as u128, NAD as u128)?)?;
        let factor = price.div(native_price)?;
        let factor = if base_to_quote { factor } else { I::UNIT.div(factor)? };
        let adjusted = mul_div_u128(input, factor.lo.max(0) as u128, ONE as u128)?.max(1);
        let native_output = self.native_seed(start, start_t, adjusted, base_to_quote)?;
        let per_input = if base_to_quote { price } else { I::UNIT.div(price)? };
        let linear = mul_div_u128(input, per_input.lo.max(0) as u128, ONE as u128)?;
        let flattening = I::UNIT.sub(
            self.exponent(start_t.lo + (start_t.hi - start_t.lo) / 2)?
                .0
                .divide_integer(2)?,
        )?;
        let extra = mul_div_u128(
            linear.saturating_sub(native_output),
            flattening.lo.max(0) as u128,
            ONE as u128,
        )?;
        let mut candidate = native_output.checked_add(extra).ok_or(ErrorCode::MarketMathOverflow)? / output_quantum;
        candidate = candidate.min(high - high / 10).max(1);
        let mut anchor = start;
        let mut cumulative = I::ZERO;
        for _ in 0..12 {
            let output = candidate
                .checked_mul(output_quantum)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let end = endpoint(output)?;
            let end_t = self.coordinate(end)?;
            let (lower, upper) = self.invariant_delta_bounds(anchor, end)?;
            let delta = cumulative.add(I { lo: lower, hi: upper })?;
            let endpoint_price = self.price_at_coordinate(end_t)?;
            // Homogeneity gives U_y=1/(y+Px), U_x=P/(y+Px).
            let opposite_scale = if base_to_quote {
                end.quote_reserve
                    .checked_add(mul_div_u128(end.base_reserve, endpoint_price.hi as u128, ONE as u128)?)
            } else {
                end.base_reserve
                    .checked_add(mul_div_u128(end.quote_reserve, ONE as u128, endpoint_price.lo as u128)?)
            }
            .ok_or(ErrorCode::MarketMathOverflow)?;
            let excess = delta.sub(I::point(required_delta))?;
            let radius = crate::math::mul_div_ceil_u128((delta.hi - delta.lo) as u128, opposite_scale, ONE as u128)?
                .div_ceil(output_quantum)
                .saturating_add(2);
            let refinement_radius = radius.max(
                output_inventory
                    .div_ceil(OUTPUT_REFINEMENT_DENOMINATOR)
                    .div_ceil(output_quantum),
            );
            let middle = excess.lo + (excess.hi - excess.lo) / 2;
            let remaining = if base_to_quote {
                end.quote_reserve
            } else {
                end.base_reserve
            };
            let curvature = I::UNIT.add(
                self.elasticity(end_t)?
                    .mul(I::ratio(opposite_scale - remaining, remaining)?)?,
            )?;
            let step = |value: i128| -> Result<u128> {
                let denominator = I::UNIT.add(I::point(value).mul(curvature)?.divide_integer(2)?)?;
                let adjusted = if denominator.lo > 0 {
                    I::point(value).div(denominator)?.abs_max()?
                } else {
                    value.checked_abs().ok_or(ErrorCode::MarketMathOverflow)?
                };
                Ok(
                    crate::math::mul_div_ceil_u128(adjusted as u128, opposite_scale, ONE as u128)?
                        .div_ceil(output_quantum)
                        .max(1),
                )
            };
            let correction = step(middle)?;
            if excess.lo >= 0 {
                low = candidate;
                accepted = Some((output, end, excess.lo));
                if correction <= refinement_radius || high - low <= refinement_radius {
                    return Ok((output, end, excess.lo));
                }
            } else {
                high = candidate.checked_sub(1).ok_or(ErrorCode::InsufficientOutputAmount)?;
            }
            if high.saturating_sub(low) <= refinement_radius {
                if let Some(accepted) = accepted {
                    return Ok(accepted);
                }
            }
            if high <= low {
                break;
            }
            // An interval straddling zero is not an accepted output. Move
            // down from its lower bound; using its positive midpoint would
            // jump to a distant bisection and discard an almost solved root.
            let proposed = if excess.lo >= 0 {
                candidate.saturating_add(correction)
            } else {
                let decrement = step(excess.lo)?.saturating_add(radius);
                candidate.saturating_sub(decrement.max(1))
            };
            candidate = if proposed > low && proposed < high {
                proposed
            } else {
                low + (high - low).div_ceil(2)
            };
            anchor = end;
            cumulative = delta;
        }
        err!(ErrorCode::InsufficientOutputAmount)
    }

    // A nonnegative lower bound certifies U(end) >= U(start). Returning
    // uncertainty is never permission to overpay the trader.
    pub(crate) fn invariant_delta_bounds(
        &self,
        start: ConcentratedCurvePoint,
        end: ConcentratedCurvePoint,
    ) -> Result<(i128, i128)> {
        if start == end {
            return Ok((0, 0));
        }
        // For nearby ownership/rounding checkpoints, enclose the line
        // integral of grad(U) directly. Along a straight reserve segment,
        // y/x is monotone; N and bounded P are monotone in that ratio
        // (lambda < 1). Thus endpoint prices enclose every interior price.
        // U_x=P/(y+Px), U_y=1/(y+Px). All reserves below are scaled by x0.
        // This avoids cancellation and repeated quadrature at atom-sized
        // ledger changes. Wider enclosures use the regular certificate.
        if end.base_reserve.abs_diff(start.base_reserve) <= start.base_reserve / 1_000
            && end.quote_reserve.abs_diff(start.quote_reserve) <= start.quote_reserve / 1_000
        {
            let x1 = I::ratio(end.base_reserve, start.base_reserve)?;
            let y0 = I::ratio(start.quote_reserve, start.base_reserve)?;
            let y1 = I::ratio(end.quote_reserve, start.base_reserve)?;
            let p0 = self.price_at_coordinate(self.coordinate(start)?)?;
            let p1 = self.price_at_coordinate(self.coordinate(end)?)?;
            let price = I {
                lo: p0.lo.min(p1.lo),
                hi: p0.hi.max(p1.hi),
            };
            let x = I {
                lo: ONE.min(x1.lo),
                hi: ONE.max(x1.hi),
            };
            let y = I {
                lo: y0.lo.min(y1.lo),
                hi: y0.hi.max(y1.hi),
            };
            let delta = price
                .mul(x1.sub(I::UNIT)?)?
                .add(y1.sub(y0)?)?
                .div(y.add(price.mul(x)?)?)?;
            if delta.hi - delta.lo <= ERROR_Q56 {
                return Ok((delta.lo, delta.hi));
            }
        }
        let x = I::ratio(end.base_reserve, start.base_reserve)?.ln()?;
        let a = self.coordinate(start)?;
        let b = self.coordinate(end)?;
        let integral = self.integral(a, b)?;
        let native = self.native_primitive(b)?.sub(self.native_primitive(a)?)?;
        let delta = x.add(native)?.add(integral)?;
        Ok((delta.lo, delta.hi))
    }
}

#[inline(never)]
fn taylor_quotient_integral(
    n: &[I; 13],
    z: &[I; 13],
    ntail: I,
    ztail: I,
    half: I,
    order: usize,
) -> Result<(I, [I; 13], i128)> {
    let mut variation = ztail.hi;
    for v in z.iter().take(order + 1).skip(1) {
        variation = checked_sum(variation, v.abs_max()?)?;
    }
    let zmin = z[0].lo.checked_sub(variation).ok_or(ErrorCode::MarketMathOverflow)?;
    require!(zmin > 0, ErrorCode::MarketMathOverflow);
    let mut q = [I::ZERO; 13];
    for k in 0..=order {
        let mut residual = n[k];
        for i in 1..=k {
            residual = residual.sub(z[i].mul(q[k - i])?)?;
        }
        q[k] = residual.div(z[0])?;
    }
    let mut estimate = I::ZERO;
    let mut qnorm = 0;
    for (k, v) in q.iter().take(order + 1).enumerate() {
        qnorm = checked_sum(qnorm, v.abs_max()?)?;
        if k % 2 == 0 {
            estimate = estimate.add(v.divide_integer(k as i128 + 1)?)?;
        }
    }
    let mut residual = I::point(qnorm).mul(ztail)?.add(ntail)?;
    for k in order + 1..=2 * order {
        let mut coefficient = I::ZERO;
        for j in k - order..=order {
            coefficient = coefficient.add(q[j].mul(z[k - j])?)?;
        }
        residual = residual.add(I::point(coefficient.abs_max()?))?;
    }
    let width = half.scale(2)?;
    let density = residual.div(I::point(zmin))?.hi;
    let error = I::point(density).mul(width)?.hi;
    Ok((estimate.mul(width)?.widen(error)?, q, density))
}

fn checked_sum(a: i128, b: i128) -> Result<i128> {
    a.checked_add(b).ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}
#[cfg(test)]
#[inline(never)]
fn reciprocal_majorant(v: &[i128; 9], order: usize) -> Result<[i128; 9]> {
    let mut result = [0_i128; 9];
    result[0] = I::UNIT.div(I::point(v[0]))?.hi;
    for k in 1..=order {
        let mut sum = 0;
        for i in 1..=k {
            sum = checked_sum(sum, interval::mul_up(v[i], result[k - i])?)?;
        }
        result[k] = interval::mul_up(result[0], sum)?;
    }
    Ok(result)
}

// Separate frame for the test-only interval derivative reference.
#[cfg(test)]
#[inline(never)]
fn residual_derivative_fraction(a: I, b: I, d: I, u: &Jet, v: &Jet, p: &Jet) -> Result<Jet> {
    let numerator = u.add(v.mul(Jet::constant(I::integer(-1)?))?)?.mul(Jet::constant(a))?;
    let denominator = u
        .add(*v)?
        .mul(Jet::constant(a))?
        .add(p.mul(Jet::constant(b))?)?
        .add(Jet::constant(d))?;
    numerator.div(denominator)
}

#[cfg(feature = "benchmark")]
pub fn bounded_swap_probe(
    amp: u64,
    core: u16,
    fade: u16,
    center: u64,
    lambda: u16,
    reference: u64,
    cap: u64,
    start: [u128; 2],
    input: u128,
    quantum: u128,
    base_to_quote: bool,
) -> Result<[u128; 3]> {
    let curve = BoundedOracleCurve::new(
        ConcentratedCurveParameters {
            peak_amplification_nad: amp.checked_mul(NAD).ok_or(ErrorCode::MarketMathOverflow)?,
            core_half_width_bps: core,
            fade_width_bps: fade,
        },
        center,
        reference,
        lambda,
        cap,
    )?;
    let (output, end) = curve.quote_exact_in(
        ConcentratedCurvePoint {
            base_reserve: start[0],
            quote_reserve: start[1],
        },
        input,
        quantum,
        base_to_quote,
    )?;
    Ok([output, end.base_reserve, end.quote_reserve])
}

// Pure diagnostic entry point used by the local SBF compute fixture. It
// neither reads an oracle nor authorizes a market transition.
#[cfg(feature = "benchmark")]
pub fn bounded_invariant_probe(
    amp: u64,
    core: u16,
    fade: u16,
    center: u64,
    lambda: u16,
    reference: u64,
    cap: u64,
    start: [u128; 2],
    end: [u128; 2],
) -> Result<(i128, i128)> {
    let curve = BoundedOracleCurve::new(
        ConcentratedCurveParameters {
            peak_amplification_nad: amp.checked_mul(NAD).ok_or(ErrorCode::MarketMathOverflow)?,
            core_half_width_bps: core,
            fade_width_bps: fade,
        },
        center,
        reference,
        lambda,
        cap,
    )?;
    let a = curve.coordinate(ConcentratedCurvePoint {
        base_reserve: start[0],
        quote_reserve: start[1],
    })?;
    let b = curve.coordinate(ConcentratedCurvePoint {
        base_reserve: end[0],
        quote_reserve: end[1],
    })?;
    let integral = curve.integral(a, b)?;
    let native = curve.native_primitive(b)?.sub(curve.native_primitive(a)?)?;
    let value = I::ratio(end[0], start[0])?.ln()?.add(native)?.add(integral)?;
    Ok((value.lo, value.hi))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn point(x: u128, y: u128) -> ConcentratedCurvePoint {
        ConcentratedCurvePoint {
            base_reserve: x,
            quote_reserve: y,
        }
    }
    #[test]
    fn exact_input_solver_certifies_integer_outputs_and_round_trips() {
        for (amp, core, fade) in [(1, 0, 0), (4, 1000, 4000), (32, 100, 0), (2000, 1, 0)] {
            for lambda in [1, 2500, 5000, 9999] {
                for reference in [6, 60, 600] {
                    let curve = BoundedOracleCurve::new(
                        ConcentratedCurveParameters {
                            peak_amplification_nad: amp * NAD,
                            core_half_width_bps: core,
                            fade_width_bps: fade,
                        },
                        60 * NAD,
                        reference * NAD,
                        lambda,
                        2 * NAD,
                    )
                    .unwrap();
                    for bps in [1, 100, 2500, 10000] {
                        for forward in [true, false] {
                            let start = ConcentratedCurvePoint {
                                base_reserve: 10_000_000_000,
                                quote_reserve: 600_000_000_000,
                            };
                            let input = (if forward {
                                start.base_reserve
                            } else {
                                start.quote_reserve
                            }) * bps
                                / 10_000;
                            let (output, end) = curve.quote_exact_in(start, input, 1, forward).unwrap_or_else(|e| {
                                panic!("amp={amp} lambda={lambda} O={reference} bps={bps} forward={forward}: {e}")
                            });
                            assert!(output > 0);
                            let certificate = curve.invariant_delta_bounds(start, end).unwrap();
                            assert!(
                                certificate.1 >= 0,
                                "independent enclosure rejected output: {certificate:?}"
                            );
                            let mut better = end;
                            let inventory = if forward {
                                start.quote_reserve
                            } else {
                                start.base_reserve
                            };
                            let increment = (output / 1_000_000).max(inventory / 100_000_000).max(100);
                            if forward {
                                better.quote_reserve -= increment;
                            } else {
                                better.base_reserve -= increment;
                            }
                            assert!(
                                curve.invariant_delta_bounds(start, better).unwrap().1 < 0,
                                "underfilled amp={amp} lambda={lambda} O={reference} bps={bps} dir={forward}: {output}"
                            );
                            let (returned, back) = curve.quote_exact_in(end, output, 1, !forward).unwrap();
                            assert!(returned<=input, "repeatable profit amp={amp} lambda={lambda} O={reference} bps={bps} dir={forward}: {returned}>{input} end={end:?} back={back:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn bounded_prices_cover_reference_examples_and_reciprocal_cap() {
        for (reference, expected) in [
            (120 * NAD, 109_544_511_501_u64),
            (900 * NAD, 200 * NAD),
            (NAD, 50 * NAD),
        ] {
            let c = BoundedOracleCurve::new(
                ConcentratedCurveParameters::cpmm(),
                100 * NAD,
                reference,
                5_000,
                2 * NAD,
            )
            .unwrap();
            let (lo, hi) = c.price_bounds_nad(point(10_000, 1_000_000)).unwrap();
            assert!(lo <= expected && hi >= expected, "{reference}: {lo}..{hi}");
            assert!(hi - lo < 100);
        }
    }
    #[test]
    fn conditional_invariant_certifies_both_sides_of_exact_cpmm_root() {
        // At lambda=.5 and O=100, sqrt(100*x)+sqrt(y) is constant.
        let c = BoundedOracleCurve::new(
            ConcentratedCurveParameters::cpmm(),
            100 * NAD,
            100 * NAD,
            5_000,
            2 * NAD,
        )
        .unwrap();
        let start = point(1_000_000, 100_000_000);
        // sqrt(100*810000)=9000, sqrt(121000000)=11000: same sum 20000.
        let end = point(810_000, 121_000_000);
        let (lo, hi) = c.invariant_delta_bounds(start, end).unwrap();
        assert!(lo <= 0 && hi >= 0, "{lo} {hi}");
        assert!(c.invariant_delta_bounds(start, point(810_000, 121_000_100)).unwrap().0 > 0);
        assert!(c.invariant_delta_bounds(start, point(810_000, 120_999_900)).unwrap().1 < 0);
        let reverse = c.invariant_delta_bounds(end, start).unwrap();
        assert!(reverse.0 <= 0 && reverse.1 >= 0);
    }
    #[test]
    fn concentrated_coordinate_and_reversibility_cross_all_bands() {
        let parameters = ConcentratedCurveParameters {
            peak_amplification_nad: 4 * NAD,
            core_half_width_bps: 1_000,
            fade_width_bps: 4_000,
        };
        let c = BoundedOracleCurve::new(parameters, 60 * NAD, 60 * NAD, 5_000, 2 * NAD).unwrap();
        let center = point(10_000_000, 600_000_000);
        let p = c.price_bounds_nad(center).unwrap();
        assert!(p.0 <= 60 * NAD && p.1 >= 60 * NAD, "{p:?}");
        for endpoint in [point(20_000_000, 300_000_000), point(5_000_000, 1_200_000_000)] {
            let forward = c.invariant_delta_bounds(center, endpoint).unwrap();
            let reverse = c.invariant_delta_bounds(endpoint, center).unwrap();
            assert!(forward.0 + reverse.0 <= 0 && forward.1 + reverse.1 >= 0);
        }
    }
    #[test]
    fn bounds_enclose_independent_high_precision_integrals() {
        let references = [
            include_str!("../../../../analysis/op-gamm-implementation-2026-10-30/invariant-reference.csv"),
            include_str!("../../../../analysis/op-gamm-implementation-2026-10-30/local-invariant-reference.csv"),
        ];
        for row in references.iter().flat_map(|rows| rows.lines().skip(1)) {
            let mut v: Vec<i128> = row.split(',').map(|x| x.parse().unwrap()).collect();
            v[10] = v[10].div_euclid(256);
            let curve = BoundedOracleCurve::new(
                ConcentratedCurveParameters {
                    peak_amplification_nad: v[0] as u64 * NAD,
                    core_half_width_bps: v[1] as u16,
                    fade_width_bps: v[2] as u16,
                },
                60 * NAD,
                v[4] as u64 * NAD,
                v[3] as u16,
                v[5] as u64,
            )
            .unwrap();
            let start = point(v[6] as u128, v[7] as u128);
            let end = point(v[8] as u128, v[9] as u128);
            let (lo, hi) = curve
                .invariant_delta_bounds(start, end)
                .unwrap_or_else(|e| panic!("{row}: {e}"));
            assert!(lo <= v[10] + 1 && hi >= v[10], "delta {row}: [{lo},{hi}]");
            let price = curve.price_bounds_nad(end).unwrap();
            assert!(
                price.0 as i128 <= v[11] + 1 && price.1 as i128 >= v[11],
                "price {row}: {price:?}"
            );
            let reverse = curve.invariant_delta_bounds(end, start).unwrap();
            assert!(lo + reverse.0 <= 0 && hi + reverse.1 >= 0, "reverse {row}");
        }
    }

    #[test]
    fn majorants_cover_independent_interval_derivative_evaluations() {
        for amp in [1, 4] {
            let curve = BoundedOracleCurve::new(
                ConcentratedCurveParameters {
                    peak_amplification_nad: amp * NAD,
                    core_half_width_bps: if amp == 1 { 0 } else { 1000 },
                    fade_width_bps: if amp == 1 { 0 } else { 4000 },
                },
                60 * NAD,
                90 * NAD,
                3700,
                2 * NAD,
            )
            .unwrap();
            for k in -30..=30 {
                let t = k * ONE / 100;
                if curve
                    .breaks
                    .iter()
                    .any(|b| t >= b.lo - ONE / 1000 && t <= b.hi + ONE / 1000)
                {
                    continue;
                }
                let cell = I {
                    lo: t - ONE / 1000,
                    hi: t + ONE / 1000,
                };
                for degree in [4, 8] {
                    let bound = curve.derivative_bound(cell, t, degree).unwrap();
                    for sample in [cell.lo, t, cell.hi] {
                        let derivative = curve.integrand(I::point(sample), t).unwrap().0[degree];
                        let mut scaled = I::point(derivative.abs_max().unwrap());
                        for _ in 0..degree {
                            scaled = scaled.mul(I::point(cell.hi - cell.lo)).unwrap();
                        }
                        assert!(
                            bound >= scaled.lo,
                            "amp={amp},t={k},degree={degree}: bound {bound}, {scaled:?}"
                        );
                    }
                }
            }
        }
    }
}
