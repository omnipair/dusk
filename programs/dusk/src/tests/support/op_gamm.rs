use interval::Jet;

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

impl BoundedOracleCurve {
    #[inline(never)]
    fn integrand(&self, t: I, region: i128) -> Result<Jet> {
        let (a, b, d) = self.coefficients(region)?;
        let (slope, offset) = self.exponent(region)?;
        let u = Jet::linear_exp(t, I::UNIT, I::ZERO)?;
        let v = Jet::linear_exp(t, slope.sub(I::UNIT)?, offset)?;
        let p = Jet::linear_exp(t, slope, offset)?;
        residual_derivative_fraction(a, b, d, &u, &v, &p)
    }

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
}
