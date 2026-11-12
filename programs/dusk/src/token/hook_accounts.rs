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
    include!("../tests/token_hook_accounts.rs");
}
