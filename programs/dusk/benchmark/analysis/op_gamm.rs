impl BoundedOracleCurve {
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

}

// Separate frame for the test-only interval derivative reference.

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
