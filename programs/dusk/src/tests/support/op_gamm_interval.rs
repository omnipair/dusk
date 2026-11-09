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
