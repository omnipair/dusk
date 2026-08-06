use anchor_lang::{prelude::*, system_program, Space};

use crate::errors::ErrorCode;

/// Calculates the total size needed for an account including the 8-byte discriminator.
///
/// @notice This function adds the 8-byte discriminator to the INIT_SPACE of type T.
/// @dev Requires T to implement the `Space` trait (via `#[derive(InitSpace)]`).
///      This correctly calculates Borsh-serialized sizes for all types including
///      `Vec`, `String`, `Option`, and `Enum` fields.
/// @return usize The total size in bytes needed for the account
pub fn get_size_with_discriminator<T: Space>() -> usize {
    8 + T::INIT_SPACE
}

/// Creates a canonical program-owned PDA while preserving `init_if_needed`
/// semantics outside Anchor's generated account parser. Callers validate the
/// PDA address before this function and deserialize the typed state after it.
pub fn initialize_pda_account_if_needed<'info>(
    payer: AccountInfo<'info>,
    account: AccountInfo<'info>,
    system_program_info: AccountInfo<'info>,
    space: usize,
    signer_seeds: &[&[u8]],
) -> Result<bool> {
    if account.owner == &crate::ID {
        require_eq!(account.data_len(), space, ErrorCode::InvalidArgument);
        return Ok(false);
    }
    require_keys_eq!(*account.owner, system_program::ID, ErrorCode::InvalidArgument);
    require_eq!(account.data_len(), 0, ErrorCode::InvalidArgument);

    let rent = Rent::get()?;
    let required_lamports = rent.minimum_balance(space).max(1);
    let signer = [signer_seeds];
    if account.lamports() == 0 {
        system_program::create_account(
            CpiContext::new(
                system_program_info,
                system_program::CreateAccount {
                    from: payer,
                    to: account,
                },
            )
            .with_signer(&signer),
            required_lamports,
            u64::try_from(space).map_err(|_| ErrorCode::MarketMathOverflow)?,
            &crate::ID,
        )?;
    } else {
        let top_up = required_lamports.saturating_sub(account.lamports());
        if top_up > 0 {
            system_program::transfer(
                CpiContext::new(
                    system_program_info.clone(),
                    system_program::Transfer {
                        from: payer,
                        to: account.clone(),
                    },
                ),
                top_up,
            )?;
        }
        system_program::allocate(
            CpiContext::new(
                system_program_info.clone(),
                system_program::Allocate {
                    account_to_allocate: account.clone(),
                },
            )
            .with_signer(&signer),
            u64::try_from(space).map_err(|_| ErrorCode::MarketMathOverflow)?,
        )?;
        system_program::assign(
            CpiContext::new(
                system_program_info,
                system_program::Assign {
                    account_to_assign: account,
                },
            )
            .with_signer(&signer),
            &crate::ID,
        )?;
    }
    Ok(true)
}
