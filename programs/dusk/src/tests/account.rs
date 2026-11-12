use super::*;

/// An existing program-owned PDA is reused only at the size this build
/// creates. One at any other size was written by a retired build: it is
/// rejected rather than grown or read at the current layout. The size
/// before `YieldAccount` gained `harvest_authority`, thirty-three bytes
/// short, is one such retired size.
#[test]
fn existing_pdas_are_reused_only_at_the_current_size() {
    let space = 500;
    for (length, reused) in [(space, true), (space - 33, false), (space + 1, false)] {
        let (key, payer_key, owner) = (Pubkey::new_unique(), Pubkey::new_unique(), crate::ID);
        let system_key = system_program::ID;
        let (mut lamports, mut payer_lamports, mut system_lamports) = (1_000_000, 1_000_000, 1);
        let (mut data, mut payer_data, mut system_data) = (vec![0_u8; length], vec![], vec![]);
        let account = AccountInfo::new(&key, false, true, &mut lamports, &mut data, &owner, false, 0);
        let payer = AccountInfo::new(&payer_key, true, true, &mut payer_lamports, &mut payer_data, &system_key, false, 0);
        let system =
            AccountInfo::new(&system_key, false, false, &mut system_lamports, &mut system_data, &system_key, true, 0);
        let result = initialize_pda_account_if_needed(payer, account, system, space, &[]);
        if reused {
            assert!(!result.unwrap(), "{length}");
        } else {
            assert!(
                matches!(result, Err(anchor_lang::error::Error::AnchorError(error)) if error.error_name == "InvalidArgument"),
                "{length}"
            );
        }
    }
}
