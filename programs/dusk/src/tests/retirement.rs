use super::*;
use anchor_lang::solana_program::program_option::COption;

#[test]
fn fixed_scope_has_no_duplicate_owner_claim_or_vault() {
    use std::collections::HashSet;
    assert_eq!(OWNERS.into_iter().collect::<HashSet<_>>().len(), 6);
    let accounts = PERSONAL
        .iter()
        .flat_map(|list| list.iter())
        .map(|item| item.address)
        .collect::<Vec<_>>();
    assert_eq!(accounts.len(), 28);
    assert_eq!(accounts.iter().collect::<HashSet<_>>().len(), 28);
    assert_eq!(VAULTS.into_iter().collect::<HashSet<_>>().len(), 8);
    let bump = [MARKET_BUMP];
    let seeds: &[&[u8]] = &[b"market_v2", MINTS[0].as_ref(), MINTS[1].as_ref(), &PARAMS_HASH, &bump];
    assert_eq!(Pubkey::create_program_address(seeds, &crate::ID).unwrap(), MARKET);
}

#[test]
fn ordered_owner_proof_matches_the_cross_language_six_owner_vector_and_rejects_mutations() {
    let decode = |text: &str| std::array::from_fn(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap());
    let root = decode("1eaa9959f116e999567c2fcc4370729ead48527f58f3731397522126b0a9e0c1");
    let proof = [
        [5; 32],
        decode("9f205e4ff748265b591e033a2c4f054a0c8fcaaa4c0ae1408bfaad8b10cb27e8"),
        decode("5d58bc22f7465f06ac66fab2e377c8b0dfb617359f1bc90516c95d5bbda3ac6b"),
    ];
    assert!(verify_owner_evidence([6; 32], 5, &proof, root));
    assert!(!verify_owner_evidence([6; 32], 4, &proof, root));
    assert!(!verify_owner_evidence([5; 32], 5, &proof, root));
    assert!(!verify_owner_evidence([6; 32], 5, &proof[..2], root));
    assert!(!verify_owner_evidence([6; 32], 6, &proof, root));
    let mut changed = proof;
    changed[0][0] ^= 1;
    assert!(!verify_owner_evidence([6; 32], 5, &changed, root));
}

#[test]
fn net_limit_is_exact_and_rounds_up_even_for_one_atom() {
    for (quoted, minimum) in [(0, 0), (1, 1), (99, 99), (100, 99), (101, 100), (10000, 9900)] {
        assert_eq!(minimum_net(quoted), minimum);
    }
    let maximum = u64::MAX;
    let minimum = minimum_net(maximum) as u128;
    assert!(minimum * 10000 >= maximum as u128 * 9900);
    assert!((minimum - 1) * 10000 < maximum as u128 * 9900);
    assert!(sum_amounts(&[maximum, 1]).is_err());
    assert!(checked_owner(6).is_err());
    assert!(checked_owner(255).is_err());
}

#[test]
fn beneficiary_token_account_rejects_delegates_foreign_close_rights_and_frozen_state() {
    let key = Pubkey::new_unique();
    let valid = TokenAccount {
        mint: MINTS[0],
        owner: OWNERS[0],
        amount: 100,
        delegate: COption::None,
        state: AccountState::Initialized,
        is_native: COption::None,
        delegated_amount: 0,
        close_authority: COption::None,
    };
    let mut examples = vec![(valid, true)];
    for change in 0..6 {
        let mut account = valid;
        match change {
            0 => account.delegate = COption::Some(Pubkey::new_unique()),
            1 => account.close_authority = COption::Some(Pubkey::new_unique()),
            2 => account.state = AccountState::Frozen,
            3 => account.owner = OWNERS[1],
            4 => account.mint = MINTS[1],
            _ => account.delegated_amount = 1,
        }
        examples.push((account, false));
    }
    for (account, accepted) in examples {
        let mut lamports = 2039280;
        let mut data = [0; TokenAccount::LEN];
        TokenAccount::pack(account, &mut data).unwrap();
        let token_program = spl_token::ID;
        let info = AccountInfo::new(&key, false, true, &mut lamports, &mut data, &token_program, false, 0);
        assert_eq!(checked_token(&info, MINTS[0], OWNERS[0]).is_ok(), accepted);
    }
}

#[test]
fn tombstone_cannot_be_decoded_as_the_old_market_and_is_bound_to_the_settlement() {
    let state = Pubkey::new_unique();
    let tombstone = RetiredLegacyMarket {
        retirement: state,
        version: 1,
    };
    let mut bytes = vec![0; 6000];
    tombstone.try_serialize(&mut bytes.as_mut_slice()).unwrap();
    assert_ne!(&bytes[..8], &hash(b"account:Market").to_bytes()[..8]);
    let parsed = RetiredLegacyMarket::try_deserialize(&mut bytes.as_slice()).unwrap();
    assert_eq!(parsed.retirement, state);
    assert_eq!(parsed.version, 1);
}
