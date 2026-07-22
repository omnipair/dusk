#![allow(unexpected_cfgs)]

use solana_program::{
    account_info::AccountInfo, entrypoint, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};
use spl_transfer_hook_interface::instruction::TransferHookInstruction;

entrypoint!(process_instruction);

fn process_instruction(
    _program_id: &Pubkey,
    _accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    match TransferHookInstruction::unpack(data) {
        Ok(TransferHookInstruction::Execute { .. }) => Ok(()),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
