//! Test-only peripheral controller: its program-owned PDA signs Dusk operator
//! instructions. The fixture requires its controller signer for every CPI.
#![allow(unexpected_cfgs)]
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
    program_error::ProgramError,
    pubkey,
    pubkey::Pubkey,
    system_instruction, system_program,
};

solana_program::declare_id!("AJZgAHa6yfsoLbJk3jzqj6dPcnei5Pb7heRMGWrZu1fQ");
const DUSK: Pubkey = pubkey!("2rYBVfGubUVbHrLeut6Dn4A9ShM695dGSPCErdsGo16Y");
entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let mut iter = accounts.iter();
    let controller = next_account_info(&mut iter)?;
    let actor = next_account_info(&mut iter)?;
    if !controller.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let (expected, bump) =
        Pubkey::find_program_address(&[b"actor", controller.key.as_ref()], program_id);
    if expected != *actor.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let bump_seed = [bump];
    let signer = [b"actor".as_ref(), controller.key.as_ref(), &bump_seed];
    match data.first() {
        Some(0) => {
            let dusk = next_account_info(&mut iter)?;
            if *dusk.key != DUSK || !dusk.executable {
                return Err(ProgramError::IncorrectProgramId);
            }
            if data.len() < 9 {
                return Err(ProgramError::InvalidInstructionData);
            }
            let forwarded = &accounts[3..];
            let instruction = Instruction {
                program_id: DUSK,
                data: data[1..].to_vec(),
                accounts: forwarded
                    .iter()
                    .map(|account| AccountMeta {
                        pubkey: *account.key,
                        is_writable: account.is_writable,
                        is_signer: account.is_signer || *account.key == expected,
                    })
                    .collect(),
            };
            let mut infos = forwarded.to_vec();
            infos.push(dusk.clone());
            invoke_signed(&instruction, &infos, &[&signer])
        }
        Some(1) if data.len() == 1 => {
            let system = next_account_info(&mut iter)?;
            if *system.key != system_program::ID
                || !actor.is_writable
                || *actor.owner != system_program::ID
                || !actor.data_is_empty()
            {
                return Err(ProgramError::InvalidArgument);
            }
            invoke_signed(
                &system_instruction::allocate(actor.key, 8),
                &[actor.clone(), system.clone()],
                &[&signer],
            )?;
            invoke_signed(
                &system_instruction::assign(actor.key, program_id),
                &[actor.clone(), system.clone()],
                &[&signer],
            )?;
            actor.try_borrow_mut_data()?.copy_from_slice(b"OPERATR1");
            Ok(())
        }
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
