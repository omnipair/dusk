#![allow(unexpected_cfgs)]

use solana_program::{
    account_info::AccountInfo, entrypoint, entrypoint::ProgramResult, program::set_return_data,
    program_error::ProgramError, pubkey::Pubkey,
};
use uint::construct_uint;

construct_uint! { pub struct U512(8); }

// At the bounded reserve range, this resolution gives >3,000 invariant
// units per smallest quote atom; root flooring cannot add one output atom.
const SCALE: u64 = 1_000_000;
const COEFF_SCALE: u64 = 1_000_000_000_000_000_000;
const SERIES_SCALE: u128 = 1_000_000_000_000_000_000;
const PRICE_SCALE: u64 = 1_000_000_000;
const MAX_RESERVE: u64 = 1_000_000_000_000_000;

entrypoint!(process_instruction);

fn fifth_power(x: U512) -> U512 {
    let x2 = x * x;
    x2 * x2 * x
}

/// Floor of the fifth root, using descending integer Newton steps.
fn fifth_root(n: U512) -> U512 {
    if n.is_zero() {
        return U512::zero();
    }
    let mut x = U512::one() << ((n.bits() + 4) / 5);
    loop {
        let x2 = x * x;
        let x4 = x2 * x2;
        let next = (x * U512::from(4u8) + n / x4) / U512::from(5u8);
        if next >= x {
            break;
        }
        x = next;
    }
    // Newton's fixed point can be one integer above the true floor.
    while fifth_power(x) > n {
        x -= U512::one();
    }
    while fifth_power(x + U512::one()) <= n {
        x += U512::one();
    }
    x
}

/// Floor(SCALE * v^(4/5)). Inputs are bounded so v^4*SCALE^5 fits U512.
fn fixed_power(v: u64, scale5: U512) -> U512 {
    let v = U512::from(v);
    let v2 = v * v;
    fifth_root(v2 * v2 * scale5)
}

fn read_u64(data: &[u8], start: usize) -> u64 {
    u64::from_le_bytes(data[start..start + 8].try_into().unwrap())
}

fn valid_output(
    out: u64,
    max_out: u64,
    initial_out_power: U512,
    gain: U512,
    other_reserve: u64,
    other_multiplier: u64,
    scale5: U512,
    coeff_gain: U512,
    coeff_loss: U512,
) -> bool {
    if out > max_out {
        return false;
    }
    let new_other = other_reserve - out * other_multiplier;
    let loss = initial_out_power - fixed_power(new_other, scale5);
    // Each floor difference differs from the real difference by < 1.
    // The extra sum of coefficients therefore makes this a sufficient
    // condition for nondecreasing conditional invariant.
    coeff_gain * gain >= coeff_loss * loss + coeff_gain + coeff_loss
}

fn quote(data: &[u8]) -> Result<u64, ProgramError> {
    if data.len() != 43 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let op = data[0];
    if op > 3 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let p0 = read_u64(data, 1);
    let pref = read_u64(data, 9);
    let base = read_u64(data, 17);
    let quote = read_u64(data, 25);
    let gross_in = read_u64(data, 33);
    let fee = u16::from_le_bytes([data[41], data[42]]) as u64;
    if p0 == 0 || pref == 0 || p0 % PRICE_SCALE != 0 || fee >= 10_000 || gross_in == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let multiplier = p0 / PRICE_SCALE;
    let x = base
        .checked_mul(multiplier)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    if x == 0 || quote == 0 || x > MAX_RESERVE || quote > MAX_RESERVE {
        return Err(ProgramError::InvalidInstructionData);
    }
    let net = (gross_in as u128 * (10_000 - fee) as u128 / 10_000) as u64;
    if net == 0 {
        return Ok(0);
    }
    let buy = op < 2;
    let pt = op % 2 == 0;
    let (input_reserve, input_delta, output_reserve, output_multiplier, max_out) = if buy {
        (quote, net, x, multiplier, base - 1)
    } else {
        let scaled_net = net
            .checked_mul(multiplier)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        (x, scaled_net, quote, 1, quote - 1)
    };
    let new_input = input_reserve
        .checked_add(input_delta)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    if new_input > MAX_RESERVE {
        return Err(ProgramError::InvalidInstructionData);
    }

    let q = U512::from(SCALE);
    let q2 = q * q;
    let scale5 = q2 * q2 * q;
    let gain = fixed_power(new_input, scale5) - fixed_power(input_reserve, scale5);
    let initial_out_power = fixed_power(output_reserve, scale5);
    let (coeff_gain, coeff_loss) = if pt {
        if buy {
            (U512::from(p0), U512::from(pref))
        } else {
            (U512::from(pref), U512::from(p0))
        }
    } else {
        // Lower/upper rational interval around sqrt(pref/p0).
        let coeff_q = U512::from(COEFF_SCALE);
        let coeff_q2 = coeff_q * coeff_q;
        let root_num = U512::from(pref) * coeff_q2 / U512::from(p0);
        let lo = root_num.integer_sqrt();
        let hi = if lo * lo * U512::from(p0) == U512::from(pref) * coeff_q2 {
            lo
        } else {
            lo + U512::one()
        };
        if buy {
            (coeff_q, hi)
        } else {
            (lo, coeff_q)
        }
    };

    // Solve f(new reserve) / f(old reserve) = 1 - t by expanding
    // (1 - t)^(5/4). This integer series only finds a nearby candidate.
    // The exact-root comparison below remains the final acceptance rule.
    let ratio_numerator = coeff_gain * gain * U512::from(SERIES_SCALE);
    let ratio_denominator = coeff_loss * initial_out_power;
    if ratio_numerator * U512::from(10u8)
        > ratio_denominator * U512::from(4u8) * U512::from(SERIES_SCALE)
    {
        return Err(ProgramError::InvalidArgument);
    }
    let t = (ratio_numerator / ratio_denominator).as_u128();
    let mut term = 5 * t / 4;
    let mut factor = SERIES_SCALE - term;
    for n in 2u128..=28 {
        let numerator = (5i128 - 4 * (n as i128 - 1)).unsigned_abs();
        term = (term * t / SERIES_SCALE) * numerator / (4 * n);
        factor += term;
        if term == 0 {
            break;
        }
    }
    let estimated_new_reserve = output_reserve as u128 * factor / SERIES_SCALE;
    let mut candidate =
        ((output_reserve as u128 - estimated_new_reserve) / output_multiplier as u128) as u64;
    candidate = candidate.min(max_out);
    let mut corrections = 0;
    while candidate > 0
        && !valid_output(
            candidate,
            max_out,
            initial_out_power,
            gain,
            output_reserve,
            output_multiplier,
            scale5,
            coeff_gain,
            coeff_loss,
        )
    {
        if corrections == 16 {
            return Err(ProgramError::InvalidArgument);
        }
        candidate -= 1;
        corrections += 1;
    }
    while candidate < max_out
        && valid_output(
            candidate + 1,
            max_out,
            initial_out_power,
            gain,
            output_reserve,
            output_multiplier,
            scale5,
            coeff_gain,
            coeff_loss,
        )
    {
        if corrections == 16 {
            return Err(ProgramError::InvalidArgument);
        }
        candidate += 1;
        corrections += 1;
    }
    Ok(candidate)
}

fn process_instruction(
    _program_id: &Pubkey,
    _accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let output = quote(data)?;
    set_return_data(&output.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifth_root_boundaries() {
        let mut cases = vec![0u64, 1, 2, 7, 8, 999, 1_000_000];
        cases.push(1_000_000_000_000_000);
        for value in cases {
            let n = U512::from(value);
            let root = fifth_root(n);
            assert!(fifth_power(root) <= n);
            assert!(fifth_power(root + U512::one()) > n);
        }
    }

    #[test]
    fn malformed_inputs_fail_closed() {
        let mut data = [0u8; 43];
        data[1..9].copy_from_slice(&100_000_000_000u64.to_le_bytes());
        data[9..17].copy_from_slice(&100_000_000_000u64.to_le_bytes());
        data[17..25].copy_from_slice(&5_000_000_000u64.to_le_bytes());
        data[25..33].copy_from_slice(&500_000_000_000u64.to_le_bytes());
        data[33..41].copy_from_slice(&1_000_000u64.to_le_bytes());
        data[41..43].copy_from_slice(&3u16.to_le_bytes());
        assert_eq!(
            quote(&data[..42]),
            Err(ProgramError::InvalidInstructionData)
        );
        assert!(quote(&data).is_ok(), "{:?}", quote(&data));
        data[0] = 4;
        assert_eq!(quote(&data), Err(ProgramError::InvalidInstructionData));
        data[0] = 0;
        data[9..17].copy_from_slice(&0u64.to_le_bytes());
        assert_eq!(quote(&data), Err(ProgramError::InvalidInstructionData));
        data[9..17].copy_from_slice(&100_000_000_000u64.to_le_bytes());
        data[33..41].copy_from_slice(&500_000_000_000u64.to_le_bytes());
        assert_eq!(quote(&data), Err(ProgramError::InvalidArgument));
    }
}
