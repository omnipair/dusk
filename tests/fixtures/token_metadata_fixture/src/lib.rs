#![allow(unexpected_cfgs)]

use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint,
    entrypoint::ProgramResult,
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction,
    sysvar::Sysvar,
};

const CREATE_V1_DISCRIMINATOR: [u8; 2] = [42, 0];
const METADATA_ACCOUNT_SPACE: usize = 1_024;

entrypoint!(process_instruction);

/// Minimal CreateV1 ABI fixture for deterministic LiteSVM tests.
///
/// Dusk's production instruction still CPIs to the canonical Metaplex program
/// ID and constructs the official CreateV1 instruction. This fixture owns that
/// ID only inside LiteSVM: it verifies the signer/PDA boundary and creates the
/// canonical metadata PDA, without pretending to implement Metaplex metadata
/// semantics that Dusk neither reads nor mutates.
fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    if data.get(..CREATE_V1_DISCRIMINATOR.len()) != Some(CREATE_V1_DISCRIMINATOR.as_slice()) {
        return Err(ProgramError::InvalidInstructionData);
    }

    let account_iter = &mut accounts.iter();
    let metadata = next_account_info(account_iter)?;
    // CreateV1 always includes the Metaplex program itself in the optional
    // master-edition account position when no edition account is requested.
    let master_edition_sentinel = next_account_info(account_iter)?;
    let mint = next_account_info(account_iter)?;
    let authority = next_account_info(account_iter)?;
    let payer = next_account_info(account_iter)?;
    let update_authority = next_account_info(account_iter)?;
    let system_program = next_account_info(account_iter)?;

    if master_edition_sentinel.key != program_id
        || !authority.is_signer
        || !payer.is_signer
        || !update_authority.is_signer
        || *system_program.key != solana_program::system_program::ID
    {
        return Err(ProgramError::InvalidAccountData);
    }

    let (expected_metadata, bump) = Pubkey::find_program_address(
        &[b"metadata", program_id.as_ref(), mint.key.as_ref()],
        program_id,
    );
    if expected_metadata != *metadata.key || !metadata.is_writable {
        return Err(ProgramError::InvalidSeeds);
    }

    let rent = Rent::get()?;
    invoke_signed(
        &system_instruction::create_account(
            payer.key,
            metadata.key,
            rent.minimum_balance(METADATA_ACCOUNT_SPACE),
            METADATA_ACCOUNT_SPACE as u64,
            program_id,
        ),
        &[payer.clone(), metadata.clone(), system_program.clone()],
        &[&[b"metadata", program_id.as_ref(), mint.key.as_ref(), &[bump]]],
    )
}
