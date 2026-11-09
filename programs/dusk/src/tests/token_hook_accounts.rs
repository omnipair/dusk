use super::*;
use spl_tlv_account_resolution::{account::ExtraAccountMeta, seeds::Seed};

fn account(key: Pubkey, data: Vec<u8>) -> AccountInfo<'static> {
    AccountInfo::new(
        Box::leak(Box::new(key)),
        false,
        true,
        Box::leak(Box::new(0)),
        Box::leak(data.into_boxed_slice()),
        Box::leak(Box::new(Pubkey::default())),
        false,
        0,
    )
}

#[test]
fn bounded_resolver_matches_spl_data_seeds_duplicates_and_privilege_rules() {
    let program_id = Pubkey::new_unique();
    let source = account(Pubkey::new_unique(), vec![1, 2, 3, 4]);
    let mint = account(Pubkey::new_unique(), vec![]);
    let destination = account(Pubkey::new_unique(), vec![]);
    let authority = account(Pubkey::new_unique(), vec![]);
    let extra = account(Pubkey::new_unique(), vec![]);
    let amount = 42_u64;
    let seeded_key =
        Pubkey::find_program_address(&[&[1, 2, 3, 4], extra.key.as_ref(), &amount.to_le_bytes()], &program_id).0;
    let mut metas = vec![
        ExtraAccountMeta::new_with_pubkey(extra.key, false, false).unwrap(),
        ExtraAccountMeta::new_with_seeds(
            &[
                Seed::AccountData {
                    account_index: 0,
                    data_index: 0,
                    length: 4,
                },
                Seed::AccountKey { index: 5 },
                Seed::InstructionData { index: 8, length: 8 },
            ],
            false,
            true,
        )
        .unwrap(),
        ExtraAccountMeta::new_with_pubkey(source.key, true, true).unwrap(),
    ];
    metas.extend((0..20).map(|_| ExtraAccountMeta::new_with_pubkey(extra.key, true, true).unwrap()));
    let mut validation_data = vec![0; ExtraAccountMetaList::size_of(metas.len()).unwrap()];
    ExtraAccountMetaList::init::<ExecuteInstruction>(&mut validation_data, &metas).unwrap();
    let validation = account(get_extra_account_metas_address(mint.key, &program_id), validation_data);
    let additional = vec![
        extra,
        source.clone(),
        account(seeded_key, vec![]),
        validation,
        account(program_id, vec![]),
    ];
    let mut original = Instruction {
        program_id: anchor_spl::token_2022::ID,
        accounts: vec![],
        data: vec![],
    };
    let mut optimized = original.clone();
    let mut original_infos = vec![source.clone(), mint.clone(), destination.clone(), authority.clone()];
    let mut optimized_infos = original_infos.clone();
    spl_transfer_hook_interface::onchain::add_extra_accounts_for_execute_cpi(
        &mut original,
        &mut original_infos,
        &program_id,
        source.clone(),
        mint.clone(),
        destination.clone(),
        authority.clone(),
        amount,
        &additional,
    )
    .unwrap();
    add_transfer_hook_accounts(
        &mut optimized,
        &mut optimized_infos,
        &program_id,
        source,
        mint,
        destination,
        authority,
        amount,
        &additional,
    )
    .unwrap();
    assert_eq!(optimized, original);
    assert_eq!(
        optimized_infos.iter().map(|info| info.key).collect::<Vec<_>>(),
        original_infos.iter().map(|info| info.key).collect::<Vec<_>>()
    );
    for info in &additional {
        assert!(info.try_borrow_mut_data().is_ok());
    }
}
