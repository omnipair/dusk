//! Centralized test publisher for devnet mock assets. Never a market-data source.
//! Each signer owns a separate feed PDA; updates preserve pair identity and use
//! the chain clock plus a checked monotone sequence. Output is exactly DOPRV001.
#[allow(deprecated)] // Solana 2.x instruction ABI used by the devnet fixture.
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint,
    entrypoint::ProgramResult,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction, system_program,
    sysvar::Sysvar,
};

const REPORT_LEN: usize = 145;
const INPUT_LEN: usize = 121;
entrypoint!(process_instruction);

fn validate(data: &[u8]) -> ProgramResult {
    if data.len() != INPUT_LEN
        || data[..32] == [0; 32]
        || data[32..64] == [0; 32]
        || data[64..96] == [0; 32]
        || data[32..64] == data[64..96]
        || data[112] > 1
    {
        return Err(ProgramError::InvalidInstructionData);
    }
    let price = u64::from_le_bytes(data[96..104].try_into().unwrap());
    let confidence = u64::from_le_bytes(data[104..112].try_into().unwrap());
    if price == 0 || confidence >= price {
        return Err(ProgramError::InvalidInstructionData);
    }
    Ok(())
}

pub fn process_instruction(
    program: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    validate(data)?;
    let mut iter = accounts.iter();
    let authority = next_account_info(&mut iter)?;
    let report = next_account_info(&mut iter)?;
    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if !report.is_writable {
        return Err(ProgramError::InvalidAccountData);
    }
    let (expected, bump) = Pubkey::find_program_address(
        &[b"mock-feed", authority.key.as_ref(), &data[..32]],
        program,
    );
    if report.key != &expected {
        return Err(ProgramError::InvalidSeeds);
    }
    let bump_bytes = [bump];
    let seeds: &[&[u8]] = &[
        b"mock-feed",
        authority.key.as_ref(),
        &data[..32],
        &bump_bytes,
    ];
    if report.owner == &system_program::id() && report.data_is_empty() {
        let system = next_account_info(&mut iter)?;
        if system.key != &system_program::id() {
            return Err(ProgramError::IncorrectProgramId);
        }
        let needed = Rent::get()?
            .minimum_balance(REPORT_LEN)
            .saturating_sub(report.lamports());
        if needed > 0 {
            invoke(
                &system_instruction::transfer(authority.key, report.key, needed),
                &[authority.clone(), report.clone(), system.clone()],
            )?;
        }
        // Allocate/assign also handles a previously prefunded PDA.
        invoke_signed(
            &system_instruction::allocate(report.key, REPORT_LEN as u64),
            &[report.clone(), system.clone()],
            &[seeds],
        )?;
        invoke_signed(
            &system_instruction::assign(report.key, program),
            &[report.clone(), system.clone()],
            &[seeds],
        )?;
    }
    if report.owner != program || report.data_len() != REPORT_LEN {
        return Err(ProgramError::InvalidAccountData);
    }
    let clock = Clock::get()?;
    let mut output = report.try_borrow_mut_data()?;
    let sequence = if output[..8] == *b"DOPRV001" {
        if output[8..104] != data[..96] {
            return Err(ProgramError::InvalidInstructionData);
        }
        u64::from_le_bytes(output[136..144].try_into().unwrap())
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?
    } else {
        if output.iter().any(|b| *b != 0) {
            return Err(ProgramError::InvalidAccountData);
        }
        1
    };
    let expected_sequence = u64::from_le_bytes(data[113..121].try_into().unwrap());
    if expected_sequence != sequence - 1 {
        return Err(ProgramError::InvalidArgument);
    }
    output[..8].copy_from_slice(b"DOPRV001");
    output[8..120].copy_from_slice(&data[..112]);
    output[120..128].copy_from_slice(&clock.unix_timestamp.to_le_bytes());
    output[128..136].copy_from_slice(&clock.slot.to_le_bytes());
    output[136..144].copy_from_slice(&sequence.to_le_bytes());
    output[144] = data[112];
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> [u8; INPUT_LEN] {
        let mut d = [0; INPUT_LEN];
        d[..32].fill(1);
        d[32..64].fill(2);
        d[64..96].fill(3);
        d[96..104].copy_from_slice(&1_000_000_000_u64.to_le_bytes());
        d[112] = 1;
        d
    }
    #[test]
    fn validates_price_pair_status_and_exact_encoding() {
        assert!(validate(&input()).is_ok());
        assert!(validate(&input()[..112]).is_err());
        let mut d = input();
        d[112] = 2;
        assert!(validate(&d).is_err());
        d = input();
        d[104..112].copy_from_slice(&1_000_000_000_u64.to_le_bytes());
        assert!(validate(&d).is_err());
        d = input();
        d[64..96].fill(2);
        assert!(validate(&d).is_err());
    }
}
