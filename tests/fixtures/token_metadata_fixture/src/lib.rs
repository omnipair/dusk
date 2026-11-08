#![allow(unexpected_cfgs)]

use borsh::{BorshDeserialize, BorshSerialize};
use mpl_token_metadata::{
    accounts::Metadata,
    instructions::{CreateV1InstructionArgs, UpdateMetadataAccountV2InstructionArgs},
    types::Key,
};
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
const UPDATE_V2_DISCRIMINATOR: u8 = 15;
const METADATA_ACCOUNT_SPACE: usize = 1_024;

entrypoint!(process_instruction);

/// Minimal CreateV1/UpdateV2 ABI fixture for deterministic LiteSVM tests.
///
/// Dusk's production instruction still CPIs to the canonical Metaplex program
/// ID and constructs the official CreateV1 instruction. This fixture owns that
/// ID only inside LiteSVM: it verifies the signer/PDA boundary and creates the
/// canonical metadata PDA. It preserves the official serialized account layout
/// and checks the signer boundary for cosmetic updates; it is not a complete
/// implementation of Metaplex asset rules.
fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    if data.first() == Some(&UPDATE_V2_DISCRIMINATOR) {
        let args = UpdateMetadataAccountV2InstructionArgs::try_from_slice(&data[1..])?;
        let account_iter = &mut accounts.iter();
        let account = next_account_info(account_iter)?;
        let authority = next_account_info(account_iter)?;
        let mut metadata = Metadata::safe_deserialize(&account.try_borrow_data()?)?;
        let (expected, _) = Pubkey::find_program_address(
            &[b"metadata", program_id.as_ref(), metadata.mint.as_ref()],
            program_id,
        );
        if account.owner != program_id
            || account.key != &expected
            || !account.is_writable
            || !authority.is_signer
            || authority.key.as_ref() != metadata.update_authority.as_ref()
            || !metadata.is_mutable
            || args.new_update_authority.is_some()
            || args.primary_sale_happened.is_some()
            || args.is_mutable.is_some()
        {
            return Err(ProgramError::InvalidAccountData);
        }
        let update = args.data.ok_or(ProgramError::InvalidInstructionData)?;
        metadata.name = update.name;
        metadata.symbol = update.symbol;
        metadata.uri = update.uri;
        metadata.seller_fee_basis_points = update.seller_fee_basis_points;
        metadata.creators = update.creators;
        metadata.collection = update.collection;
        metadata.uses = update.uses;
        metadata.serialize(&mut &mut account.try_borrow_mut_data()?[..])?;
        return Ok(());
    }
    if data.get(..CREATE_V1_DISCRIMINATOR.len()) != Some(CREATE_V1_DISCRIMINATOR.as_slice()) {
        return Err(ProgramError::InvalidInstructionData);
    }
    let args = CreateV1InstructionArgs::try_from_slice(&data[2..])?;

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
    )?;
    Metadata {
        key: Key::MetadataV1,
        update_authority: update_authority.key.to_bytes().into(),
        mint: mint.key.to_bytes().into(),
        name: args.name,
        symbol: args.symbol,
        uri: args.uri,
        seller_fee_basis_points: args.seller_fee_basis_points,
        creators: args.creators,
        primary_sale_happened: args.primary_sale_happened,
        is_mutable: args.is_mutable,
        edition_nonce: None,
        token_standard: Some(args.token_standard),
        collection: args.collection,
        uses: args.uses,
        collection_details: args.collection_details,
        programmable_config: None,
    }
    .serialize(&mut &mut metadata.try_borrow_mut_data()?[..])?;
    Ok(())
}
