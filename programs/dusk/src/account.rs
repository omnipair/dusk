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

/// Whether a program-owned PDA has to grow before it can be opened.
///
/// A PDA created by an earlier build of this program is smaller than the
/// state it now holds, because the state gained a field. Refusing it bricks
/// the account: every instruction that opens it fails, and the holder can no
/// longer deposit, harvest or withdraw. Growing it in place is safe because
/// Borsh appends -- the bytes already written keep their meaning, and the
/// zeroed tail decodes as the default of whatever was added, which for an
/// `Option` is `None`.
///
/// Shrinking is refused. It is not the same operation: it would truncate
/// live state rather than leave room for more of it.
pub fn existing_pda_needs_growth(current: usize, space: usize) -> Result<bool> {
    if current == space {
        return Ok(false);
    }
    require_gte!(space, current, ErrorCode::InvalidArgument);
    Ok(true)
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
        if existing_pda_needs_growth(account.data_len(), space)? {
            // Rent first. An account left below the minimum for its new size
            // is collectable, and the realloc itself would not fail.
            let rent = Rent::get()?;
            let required_lamports = rent.minimum_balance(space);
            let top_up = required_lamports.saturating_sub(account.lamports());
            if top_up > 0 {
                system_program::transfer(
                    CpiContext::new(
                        system_program_info,
                        system_program::Transfer {
                            from: payer,
                            to: account.clone(),
                        },
                    ),
                    top_up,
                )?;
            }
            account.realloc(space, true)?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `YieldAccount` before and after it gained `harvest_authority`, which
    /// is an `Option<Pubkey>`: one discriminant byte and thirty-two of key.
    /// Every yield account on the devnet market was created at the smaller
    /// size, and adding liquidity through one failed with `InvalidArgument`
    /// until this grew instead of rejecting.
    const YIELD_ACCOUNT_BEFORE: usize = 234;
    const YIELD_ACCOUNT_AFTER: usize = 267;

    #[test]
    fn a_pda_at_the_current_size_is_left_alone() {
        assert!(!existing_pda_needs_growth(YIELD_ACCOUNT_AFTER, YIELD_ACCOUNT_AFTER).unwrap());
    }

    #[test]
    fn a_pda_from_before_a_new_field_grows() {
        assert_eq!(YIELD_ACCOUNT_AFTER - YIELD_ACCOUNT_BEFORE, 1 + 32);
        assert!(existing_pda_needs_growth(YIELD_ACCOUNT_BEFORE, YIELD_ACCOUNT_AFTER).unwrap());
    }

    #[test]
    fn a_pda_larger_than_its_state_is_still_refused() {
        // Shrinking would truncate whatever those bytes hold, so this stays
        // an error rather than becoming a silent narrowing.
        assert!(existing_pda_needs_growth(YIELD_ACCOUNT_AFTER, YIELD_ACCOUNT_BEFORE).is_err());
    }
}
