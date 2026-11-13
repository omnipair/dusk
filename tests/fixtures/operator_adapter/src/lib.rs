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
const DUSK: Pubkey = pubkey!("G6VzihcGWCxt1nujxtKPB4oUYz36GPR2t5mLdXEH8yYN");
entrypoint!(process_instruction);

pub fn process_instruction<'info>(
    program_id: &Pubkey,
    accounts: &[AccountInfo<'info>],
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
        Some(0 | 2) => {
            let dusk = next_account_info(&mut iter)?;
            if *dusk.key != DUSK || !dusk.executable {
                return Err(ProgramError::IncorrectProgramId);
            }
            if data.len() < 9 {
                return Err(ProgramError::InvalidInstructionData);
            }
            let forward = |accounts: &[AccountInfo<'info>], data: &[u8]| {
                let instruction = Instruction {
                    program_id: DUSK,
                    data: data.to_vec(),
                    accounts: accounts
                        .iter()
                        .map(|account| AccountMeta {
                            pubkey: *account.key,
                            is_writable: account.is_writable,
                            is_signer: account.is_signer || *account.key == expected,
                        })
                        .collect(),
                };
                let mut infos = accounts.to_vec();
                infos.push(dusk.clone());
                invoke_signed(&instruction, &infos, &[&signer])
            };

            let forwarded = &accounts[3..];
            if data[0] == 2 {
                // Split the two account lists and instruction payloads at
                // the offsets given in the fixture's header.
                let first_accounts = usize::from(data[1]);
                let first_data_len = usize::from(data[2]);
                let data = &data[3..];
                if first_accounts == 0
                    || first_accounts >= forwarded.len()
                    || first_data_len == 0
                    || first_data_len >= data.len()
                {
                    return Err(ProgramError::InvalidInstructionData);
                }
                let (first, second) = forwarded.split_at(first_accounts);
                let (first_data, second_data) = data.split_at(first_data_len);
                forward(first, first_data)?;
                return forward(second, second_data);
            }
            forward(forwarded, &data[1..])
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
        Some(3) => {
            let adapter = accounts.last().ok_or(ProgramError::NotEnoughAccountKeys)?;
            if adapter.key != program_id || !adapter.executable {
                return Err(ProgramError::IncorrectProgramId);
            }
            let mut inner_data = data.to_vec();
            inner_data[0] = 0;
            let instruction = Instruction {
                program_id: *program_id,
                data: inner_data,
                accounts: accounts[..accounts.len() - 1]
                    .iter()
                    .map(|account| AccountMeta {
                        pubkey: *account.key,
                        is_writable: account.is_writable,
                        is_signer: account.is_signer,
                    })
                    .collect(),
            };
            invoke_signed(&instruction, accounts, &[])
        }
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
