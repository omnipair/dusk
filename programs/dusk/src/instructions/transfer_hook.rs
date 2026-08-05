use anchor_lang::{prelude::*, solana_program::program_error::ProgramError};
use anchor_spl::token_2022::{
    spl_token_2022::{
        extension::{transfer_hook::TransferHookAccount, BaseStateWithExtensions, StateWithExtensions},
        state::Account as SplTokenAccount,
    },
    Token2022,
};
use spl_transfer_hook_interface::{get_extra_account_metas_address, instruction::TransferHookInstruction};

use crate::{
    constants::YIELD_ACCOUNT_SEED_PREFIX,
    errors::ErrorCode,
    instructions::common::validate_canonical_lp_token_account_key,
    state::{Market, YieldAccount, YieldTokenKind},
};

const TRANSFER_HOOK_ACCOUNT_COUNT: usize = 12;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TokenAccountSnapshot {
    owner: Pubkey,
    mint: Pubkey,
    amount: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TransferBalances {
    source_pre_balance: u64,
    destination_pre_balance: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct YieldContext {
    pub(crate) lp_mint: Pubkey,
    pub(crate) asset_mint: Pubkey,
    pub(crate) token_kind: YieldTokenKind,
    pub(crate) swap_fee_growth_index_q64: u128,
    pub(crate) interest_growth_index_q64: u128,
}

#[derive(Clone, Copy)]
pub(crate) struct YieldContexts {
    pub(crate) items: [Option<YieldContext>; 2],
}

pub fn handle_transfer_hook<'info>(
    program_id: &Pubkey,
    accounts: &'info [AccountInfo<'info>],
    data: &[u8],
) -> Result<()> {
    let amount = match TransferHookInstruction::unpack(data).map_err(|_| error!(ErrorCode::InvalidArgument))? {
        TransferHookInstruction::Execute { amount } => amount,
        _ => return err!(ErrorCode::InvalidArgument),
    };
    require_gte!(accounts.len(), TRANSFER_HOOK_ACCOUNT_COUNT, ErrorCode::InvalidArgument);
    let lp_mint = *accounts[1].key;
    let (source_owner, destination_owner, balances) = {
        let source_token = parse_transferring_token_account(&accounts[0])?;
        let destination_token = parse_transferring_token_account(&accounts[2])?;
        require_keys_eq!(source_token.mint, lp_mint, ErrorCode::InvalidMint);
        require_keys_eq!(destination_token.mint, lp_mint, ErrorCode::InvalidMint);
        validate_canonical_lp_token_account_key(*accounts[0].key, source_token.owner, lp_mint)?;
        validate_canonical_lp_token_account_key(*accounts[2].key, destination_token.owner, lp_mint)?;
        require!(
            source_token.owner != destination_token.owner,
            ErrorCode::InvalidTokenAccount
        );
        (
            source_token.owner,
            destination_token.owner,
            TransferBalances {
                source_pre_balance: source_token
                    .amount
                    .checked_add(amount)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                destination_pre_balance: destination_token
                    .amount
                    .checked_sub(amount)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
            },
        )
    };
    require_keys_eq!(
        *accounts[4].key,
        get_extra_account_metas_address(&lp_mint, program_id),
        ErrorCode::InvalidArgument
    );
    require_keys_eq!(*accounts[4].owner, *program_id, ErrorCode::InvalidArgument);
    let market_key = *accounts[5].key;
    require_keys_eq!(*accounts[5].owner, *program_id, ErrorCode::InvalidMarket);
    require!(accounts[5].is_writable, ErrorCode::InvalidMarket);
    let (base_context, quote_context) =
        mutate_program_owned_account::<Market, _, _>(&accounts[5], ErrorCode::InvalidMarket, |market| {
            require_keys_eq!(*accounts[6].key, market.base_side.asset_mint, ErrorCode::InvalidMint);
            require_keys_eq!(*accounts[7].key, market.quote_side.asset_mint, ErrorCode::InvalidMint);
            let contexts = current_yield_contexts(market, lp_mint)?.ok_or(error!(ErrorCode::InvalidMint))?;
            Ok((
                contexts.items[0].ok_or(error!(ErrorCode::InvalidYieldAccount))?,
                contexts.items[1].ok_or(error!(ErrorCode::InvalidYieldAccount))?,
            ))
        })?;
    checkpoint_transfer_party(
        &accounts[8],
        &accounts[10],
        program_id,
        source_owner,
        market_key,
        base_context,
        quote_context,
        balances.source_pre_balance,
    )?;
    checkpoint_transfer_party(
        &accounts[9],
        &accounts[11],
        program_id,
        destination_owner,
        market_key,
        base_context,
        quote_context,
        balances.destination_pre_balance,
    )
}

/// Reusable SBF stack boundary for mutable raw Anchor accounts supplied to the
/// Token-2022 callback.
///
/// Anchor's ordinary instruction dispatcher owns this deserialize/serialize
/// boundary. Transfer hooks enter through a raw `AccountInfo` slice instead,
/// and large account temporaries must not share the callback's 4 KiB frame.
/// The same boundary owns both `Market` and `YieldAccount` round trips, so the
/// stack isolation is a real callback abstraction rather than a one-use shim.
#[inline(never)]
fn mutate_program_owned_account<T, R, F>(account_info: &AccountInfo, invalid_account: ErrorCode, mutate: F) -> Result<R>
where
    T: AccountDeserialize + AccountSerialize,
    F: FnOnce(&mut T) -> Result<R>,
{
    let mut data = account_info.try_borrow_mut_data()?;
    let mut cursor: &[u8] = &data;
    let mut account = Box::new(T::try_deserialize(&mut cursor).map_err(|_| error!(invalid_account))?);
    let output = mutate(&mut account)?;
    let mut write_cursor = &mut data[..];
    account
        .try_serialize(&mut write_cursor)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    Ok(output)
}

fn parse_transferring_token_account(info: &AccountInfo) -> Result<TokenAccountSnapshot> {
    require_keys_eq!(*info.owner, Token2022::id(), ErrorCode::InvalidTokenAccount);
    let data = info.try_borrow_data()?;
    let token_account =
        StateWithExtensions::<SplTokenAccount>::unpack(&data).map_err(|_| error!(ErrorCode::InvalidTokenAccount))?;
    let hook_account = token_account
        .get_extension::<TransferHookAccount>()
        .map_err(|_| error!(ErrorCode::InvalidTokenAccount))?;
    require!(bool::from(hook_account.transferring), ErrorCode::InvalidTokenAccount);
    Ok(TokenAccountSnapshot {
        owner: token_account.base.owner,
        mint: token_account.base.mint,
        amount: token_account.base.amount,
    })
}

pub(crate) fn current_yield_contexts(market: &mut Market, lp_mint: Pubkey) -> Result<Option<YieldContexts>> {
    if market.ylp_mint == lp_mint {
        market.base_side.carry_forward_swap_fees()?;
        market.base_side.carry_forward_interest()?;
        market.quote_side.carry_forward_swap_fees()?;
        market.quote_side.carry_forward_interest()?;
        return Ok(Some(YieldContexts {
            items: [
                Some(YieldContext {
                    lp_mint,
                    asset_mint: market.base_side.asset_mint,
                    token_kind: YieldTokenKind::Ylp,
                    swap_fee_growth_index_q64: market.base_side.fees.swap_fee_growth_index_q64,
                    interest_growth_index_q64: market.base_side.fees.interest_growth_index_q64,
                }),
                Some(YieldContext {
                    lp_mint,
                    asset_mint: market.quote_side.asset_mint,
                    token_kind: YieldTokenKind::Ylp,
                    swap_fee_growth_index_q64: market.quote_side.fees.swap_fee_growth_index_q64,
                    interest_growth_index_q64: market.quote_side.fees.interest_growth_index_q64,
                }),
            ],
        }));
    }
    if market.base_side.hlp_mint == lp_mint {
        market.checkpoint_hlp_yield_from_ylp(crate::state::MarketAsset::Base)?;
        return Ok(Some(YieldContexts {
            items: [
                Some(YieldContext {
                    lp_mint,
                    asset_mint: market.base_side.asset_mint,
                    token_kind: YieldTokenKind::Hlp,
                    swap_fee_growth_index_q64: market.base_hlp_vault.base_swap_fee_growth_index_q64,
                    interest_growth_index_q64: market.base_hlp_vault.base_interest_growth_index_q64,
                }),
                Some(YieldContext {
                    lp_mint,
                    asset_mint: market.quote_side.asset_mint,
                    token_kind: YieldTokenKind::Hlp,
                    swap_fee_growth_index_q64: market.base_hlp_vault.quote_swap_fee_growth_index_q64,
                    interest_growth_index_q64: market.base_hlp_vault.quote_interest_growth_index_q64,
                }),
            ],
        }));
    }
    if market.quote_side.hlp_mint == lp_mint {
        market.checkpoint_hlp_yield_from_ylp(crate::state::MarketAsset::Quote)?;
        return Ok(Some(YieldContexts {
            items: [
                Some(YieldContext {
                    lp_mint,
                    asset_mint: market.base_side.asset_mint,
                    token_kind: YieldTokenKind::Hlp,
                    swap_fee_growth_index_q64: market.quote_hlp_vault.base_swap_fee_growth_index_q64,
                    interest_growth_index_q64: market.quote_hlp_vault.base_interest_growth_index_q64,
                }),
                Some(YieldContext {
                    lp_mint,
                    asset_mint: market.quote_side.asset_mint,
                    token_kind: YieldTokenKind::Hlp,
                    swap_fee_growth_index_q64: market.quote_hlp_vault.quote_swap_fee_growth_index_q64,
                    interest_growth_index_q64: market.quote_hlp_vault.quote_interest_growth_index_q64,
                }),
            ],
        }));
    }
    Ok(None)
}

fn checkpoint_transfer_party<'info>(
    base_account_info: &AccountInfo<'info>,
    quote_account_info: &AccountInfo<'info>,
    program_id: &Pubkey,
    owner: Pubkey,
    market: Pubkey,
    base_context: YieldContext,
    quote_context: YieldContext,
    pre_transfer_balance: u64,
) -> Result<()> {
    for (account_info, yield_context) in [(base_account_info, base_context), (quote_account_info, quote_context)] {
        require!(account_info.is_writable, ErrorCode::InvalidYieldAccount);
        require_keys_eq!(*account_info.owner, *program_id, ErrorCode::InvalidYieldAccount);
        mutate_program_owned_account::<YieldAccount, _, _>(
            account_info,
            ErrorCode::InvalidYieldAccount,
            |yield_account| {
                yield_account.assert_account(
                    owner,
                    market,
                    yield_context.lp_mint,
                    yield_context.asset_mint,
                    yield_context.token_kind,
                )?;
                let (expected_key, expected_bump) = Pubkey::find_program_address(
                    &[
                        YIELD_ACCOUNT_SEED_PREFIX,
                        market.as_ref(),
                        owner.as_ref(),
                        yield_context.lp_mint.as_ref(),
                        yield_context.asset_mint.as_ref(),
                        &[yield_context.token_kind.code()],
                    ],
                    program_id,
                );
                require_keys_eq!(*account_info.key, expected_key, ErrorCode::InvalidYieldAccount);
                require_eq!(yield_account.bump, expected_bump, ErrorCode::InvalidYieldAccount);
                yield_account.accrue(
                    pre_transfer_balance,
                    yield_context.swap_fee_growth_index_q64,
                    yield_context.interest_growth_index_q64,
                )
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!("../tests/instructions/transfer_hook.rs");
}
