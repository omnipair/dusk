use super::*;
use anchor_lang::solana_program::{program::invoke_signed, program_option::COption};

pub fn checked_mint(info: &AccountInfo, expected: Pubkey) -> Result<()> {
    require_keys_eq!(info.key(), expected, RetirementError::Scope);
    require_keys_eq!(*info.owner, spl_token::ID, RetirementError::Scope);
    let mint = Mint::unpack(&info.try_borrow_data()?)?;
    require!(mint.decimals == 6, RetirementError::Scope);
    Ok(())
}

pub fn checked_token(info: &AccountInfo, mint: Pubkey, owner: Pubkey) -> Result<TokenAccount> {
    require_keys_eq!(*info.owner, spl_token::ID, RetirementError::Recipient);
    let account = TokenAccount::unpack(&info.try_borrow_data()?)?;
    require!(
        account.mint == mint
            && account.owner == owner
            && account.state == AccountState::Initialized
            && account.delegate == COption::None
            && account.delegated_amount == 0
            && account.is_native == COption::None
            && (account.close_authority == COption::None || account.close_authority == COption::Some(owner)),
        RetirementError::Recipient
    );
    Ok(account)
}

pub fn sum_amounts(amounts: &[u64]) -> Result<u64> {
    amounts.iter().try_fold(0u64, |total, amount| {
        total.checked_add(*amount).ok_or_else(|| error!(RetirementError::Math))
    })
}

pub fn transfer<'info>(
    from: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    market: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let bump = [MARKET_BUMP];
    let seeds: &[&[u8]] = &[b"market_v2", MINTS[0].as_ref(), MINTS[1].as_ref(), &PARAMS_HASH, &bump];
    let derived = Pubkey::create_program_address(seeds, &crate::ID).map_err(|_| error!(RetirementError::Scope))?;
    require_keys_eq!(derived, MARKET, RetirementError::Scope);
    let ix = spl_token::instruction::transfer_checked(
        &spl_token::ID,
        from.key,
        mint.key,
        to.key,
        market.key,
        &[],
        amount,
        6,
    )?;
    invoke_signed(
        &ix,
        &[
            from.clone(),
            mint.clone(),
            to.clone(),
            market.clone(),
            token_program.clone(),
        ],
        &[seeds],
    )?;
    Ok(())
}

pub fn pay_tokens(ctx: &Context<PayLegacyRetirement>, owner: Pubkey, amounts: [u64; 2]) -> Result<()> {
    let a = &ctx.accounts;
    for (side, vault, mint, recipient) in [
        (0, &a.base_vault, &a.base_mint, &a.base_recipient),
        (1, &a.quote_vault, &a.quote_mint, &a.quote_recipient),
    ] {
        checked_mint(mint, MINTS[side])?;
        let source = checked_token(vault, MINTS[side], MARKET)?;
        let before = checked_token(recipient, MINTS[side], owner)?.amount;
        require!(source.amount >= amounts[side], RetirementError::Funding);
        transfer(
            vault,
            mint,
            recipient,
            &a.market.to_account_info(),
            &a.token_program.to_account_info(),
            amounts[side],
        )?;
        let after = checked_token(recipient, MINTS[side], owner)?.amount;
        require!(
            after.checked_sub(before) == Some(amounts[side]),
            RetirementError::Minimum
        );
    }
    Ok(())
}
