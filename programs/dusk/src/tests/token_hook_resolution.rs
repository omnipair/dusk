use spl_tlv_account_resolution::{account::ExtraAccountMeta, seeds::Seed, pubkey_data::PubkeyData, state::ExtraAccountMetaList};
use spl_transfer_hook_interface::{instruction as hook, get_extra_account_metas_address};

fn test_info(key: Pubkey, data: Vec<u8>) -> AccountInfo<'static> {
    AccountInfo::new(Box::leak(Box::new(key)), false, true, Box::leak(Box::new(1)),
        Box::leak(data.into_boxed_slice()), Box::leak(Box::new(Pubkey::default())), false, 0)
}

#[test]
fn hook_resolver_matches_spl_for_dynamic_seeds_duplicates_and_privileges() {
    for mask in 0..16 {
        let program = Pubkey::new_unique();
        let external = Pubkey::new_unique();
        let key = Pubkey::new_unique();
        let source = test_info(Pubkey::new_unique(), key.to_bytes().to_vec());
        let mint = test_info(Pubkey::new_unique(), vec![0; 128]);
        let destination = test_info(Pubkey::new_unique(), vec![0; 128]);
        let authority = test_info(Pubkey::new_unique(), vec![]);
        let amount = 731_u64;
        let seeded = Pubkey::find_program_address(&[source.key.as_ref(), &amount.to_le_bytes(), &key.to_bytes()[..4]], &program).0;
        let chained = Pubkey::find_program_address(&[seeded.as_ref()], &external).0;
        let metas = vec![
            ExtraAccountMeta::new_with_pubkey(source.key, mask & 1 != 0, mask & 2 != 0).unwrap(),
            ExtraAccountMeta::new_with_pubkey(&external, false, false).unwrap(),
            ExtraAccountMeta::new_with_seeds(&[
                Seed::AccountKey {index: 0}, Seed::InstructionData {index: 8, length: 8},
                Seed::AccountData {account_index: 0, data_index: 0, length: 4},
            ], false, true).unwrap(),
            ExtraAccountMeta::new_external_pda_with_seeds(6, &[Seed::AccountKey {index: 7}], false, true).unwrap(),
            ExtraAccountMeta::new_with_pubkey_data(&PubkeyData::AccountData {account_index: 0, data_index: 0}, false, false).unwrap(),
            ExtraAccountMeta::new_with_pubkey(&key, mask & 4 != 0, mask & 8 != 0).unwrap(),
        ];
        let mut data = vec![0; ExtraAccountMetaList::size_of(metas.len()).unwrap()];
        ExtraAccountMetaList::init::<hook::ExecuteInstruction>(&mut data, &metas).unwrap();
        let validation = test_info(get_extra_account_metas_address(mint.key, &program), data);
        let additional = vec![test_info(program, vec![]), validation, source.clone(), test_info(external, vec![]),
            test_info(seeded, vec![]), test_info(chained, vec![]), test_info(key, vec![])];
        let initial = spl_token_2022::instruction::transfer_checked(&Token2022::id(), source.key, mint.key,
            destination.key, authority.key, &[], amount, 6).unwrap();
        for missing in 0..=additional.len() {
            let supplied: Vec<_> = additional.iter().enumerate().filter(|(i, _)| *i != missing).map(|(_, a)| a.clone()).collect();
            let mut reference = initial.clone();
            let mut result = initial.clone();
            let mut ref_infos = vec![source.clone(), mint.clone(), destination.clone(), authority.clone()];
            let mut result_infos = ref_infos.clone();
            let expected = spl_transfer_hook_interface::onchain::add_extra_accounts_for_execute_cpi(
                &mut reference, &mut ref_infos, &program, source.clone(), mint.clone(), destination.clone(), authority.clone(), amount, &supplied);
            let actual = add_transfer_hook_accounts(&mut result, &mut result_infos, &program,
                source.clone(), mint.clone(), destination.clone(), authority.clone(), amount, &supplied)
                .map_err(ProgramError::from);
            assert_eq!(actual, expected);
            if actual.is_ok() {
                assert_eq!(result, reference);
                assert_eq!(result_infos.iter().map(|a| a.key).collect::<Vec<_>>(), ref_infos.iter().map(|a| a.key).collect::<Vec<_>>());
            }
        }
    }
}
