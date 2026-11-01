//! Transfer-hook resolution with one borrow table. The SPL resolver allocates
//! another growing borrow table for every meta; Solana's bump allocator cannot
//! reclaim those temporary tables. Resolution and privilege rules are unchanged.
use anchor_lang::{
    prelude::*,
    solana_program::instruction::{AccountMeta, Instruction},
};
use spl_tlv_account_resolution::{error::AccountResolutionError, state::ExtraAccountMetaList};
use spl_transfer_hook_interface::{
    error::TransferHookError,
    get_extra_account_metas_address,
    instruction::{self, ExecuteInstruction},
};
use spl_type_length_value::state::TlvStateBorrowed;

#[allow(clippy::too_many_arguments)]
pub fn add_transfer_hook_accounts<'a>(
    cpi_instruction: &mut Instruction,
    cpi_account_infos: &mut Vec<AccountInfo<'a>>,
    program_id: &Pubkey,
    source: AccountInfo<'a>,
    mint: AccountInfo<'a>,
    destination: AccountInfo<'a>,
    authority: AccountInfo<'a>,
    amount: u64,
    additional: &[AccountInfo<'a>],
) -> Result<()> {
    let validation_key = get_extra_account_metas_address(mint.key, program_id);
    let program = additional
        .iter()
        .find(|info| info.key == program_id)
        .ok_or_else(|| ProgramError::from(TransferHookError::IncorrectAccount))?;
    if let Some(validation) = additional.iter().find(|info| *info.key == validation_key) {
        let data = validation.try_borrow_data()?;
        let state = TlvStateBorrowed::unpack(&data)?;
        let metas = ExtraAccountMetaList::unpack_with_tlv_state::<ExecuteInstruction>(&state)?;
        let mut execute =
            instruction::execute(program_id, source.key, mint.key, destination.key, authority.key, amount);
        execute.accounts.reserve(metas.data().len() + 1);
        execute.accounts.push(AccountMeta::new_readonly(validation_key, false));
        let mut available = Vec::with_capacity(5 + additional.len());
        available.extend([source, mint, destination, authority, validation.clone()]);
        available.extend_from_slice(additional);
        let borrowed = available
            .iter()
            .map(AccountInfo::try_borrow_data)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut resolved = Vec::with_capacity(5 + metas.data().len());
        resolved.extend(0..5);
        for extra in metas.data() {
            let mut meta = extra.resolve(&execute.data, program_id, |index| {
                resolved
                    .get(index)
                    .map(|i| (available[*i].key, Some(borrowed[*i].as_ref())))
            })?;
            // Exactly SPL's duplicate-account de-escalation: a later meta may
            // not add privilege that an earlier occurrence did not possess.
            let highest =
                execute
                    .accounts
                    .iter()
                    .filter(|old| old.pubkey == meta.pubkey)
                    .fold(None, |previous, old| {
                        let (signer, writable) = previous.unwrap_or((false, false));
                        Some((signer || old.is_signer, writable || old.is_writable))
                    });
            if let Some((signer, writable)) = highest {
                meta.is_signer &= signer;
                meta.is_writable &= writable;
            }
            // SPL resolves extra accounts only from the caller's additional list.
            let index = additional
                .iter()
                .position(|info| *info.key == meta.pubkey)
                .ok_or_else(|| ProgramError::from(AccountResolutionError::IncorrectAccount))?
                + 5;
            resolved.push(index);
            execute.accounts.push(meta);
        }
        cpi_instruction.accounts.extend_from_slice(&execute.accounts[5..]);
        cpi_account_infos.reserve(metas.data().len() + 2);
        cpi_account_infos.extend(resolved[5..].iter().map(|i| available[*i].clone()));
        cpi_instruction
            .accounts
            .push(AccountMeta::new_readonly(validation_key, false));
        cpi_account_infos.push(validation.clone());
    }
    cpi_instruction
        .accounts
        .push(AccountMeta::new_readonly(*program_id, false));
    cpi_account_infos.push(program.clone());
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
