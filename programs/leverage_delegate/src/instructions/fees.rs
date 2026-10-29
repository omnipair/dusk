use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount};
use dusk::{constants::FUTARCHY_AUTHORITY_SEED_PREFIX, state::FutarchyAuthority};

use crate::{constants::*, errors::LeverageDelegateError, token::*};

/// Rounded up in raw token units so splitting orders cannot avoid the fee.
pub fn order_protocol_fee(value: u64) -> u64 {
    ((value as u128 * ORDER_PROTOCOL_FEE_BPS as u128).div_ceil(10_000)) as u64
}

pub fn gross_debit_for_net(mint: &AccountInfo, net: u64, epoch: u64) -> Result<u64> {
    if net == 0 {
        return Ok(0);
    }
    net.checked_add(dusk::token::get_transfer_inverse_fee_for_epoch(
        mint, net, epoch,
    )?)
    .ok_or_else(|| error!(LeverageDelegateError::MathOverflow))
}

/// Additional protocol fee paid by the order owner, separately from keeper rewards.
#[derive(Accounts)]
pub struct OrderFeePayment<'info> {
    #[account(mut)]
    pub fee_recipient: Box<InterfaceAccount<'info, TokenAccount>>,
}

impl<'info> OrderFeePayment<'info> {
    #[inline(never)]
    pub fn collect(
        &mut self,
        owner: Pubkey,
        order: Pubkey,
        mint: &InterfaceAccount<'info, Mint>,
        futarchy: &Account<'info, FutarchyAuthority>,
        value: u64,
        source: AccountInfo<'info>,
        authority: AccountInfo<'info>,
        signer: &[&[&[u8]]],
        token_program: &AccountInfo<'info>,
        token_2022_program: &AccountInfo<'info>,
        remaining: &[AccountInfo<'info>],
    ) -> Result<u64> {
        self.collect_with_budget(
            owner,
            order,
            mint,
            futarchy,
            value,
            u64::MAX,
            source,
            authority,
            signer,
            token_program,
            token_2022_program,
            remaining,
        )
    }

    /// Collect as much of the order fee as the caller can fund after its
    /// guaranteed owner and executor payouts. A fee cannot strand an exit.
    #[inline(never)]
    pub fn collect_with_budget(
        &mut self,
        owner: Pubkey,
        order: Pubkey,
        mint: &InterfaceAccount<'info, Mint>,
        futarchy: &Account<'info, FutarchyAuthority>,
        value: u64,
        max_debit: u64,
        source: AccountInfo<'info>,
        authority: AccountInfo<'info>,
        signer: &[&[&[u8]]],
        token_program: &AccountInfo<'info>,
        token_2022_program: &AccountInfo<'info>,
        remaining: &[AccountInfo<'info>],
    ) -> Result<u64> {
        require_keys_eq!(
            futarchy.key(),
            Pubkey::find_program_address(&[FUTARCHY_AUTHORITY_SEED_PREFIX], &dusk::ID,).0,
            LeverageDelegateError::InvalidOrder
        );
        require_keys_eq!(
            self.fee_recipient.owner,
            futarchy.recipients.futarchy_treasury,
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_eq!(
            self.fee_recipient.mint,
            mint.key(),
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_neq!(
            source.key(),
            self.fee_recipient.key(),
            LeverageDelegateError::InvalidTokenAccount
        );
        let assessed_fee = order_protocol_fee(value);
        let epoch = Clock::get()?.epoch;
        let full_debit = gross_debit_for_net(&mint.to_account_info(), assessed_fee, epoch)?;
        let mut debit = full_debit.min(max_debit);
        let mut fee = if debit > 0 {
            debit
                .checked_sub(dusk::token::get_transfer_fee_for_epoch(
                    &mint.to_account_info(),
                    debit,
                    epoch,
                )?)
                .ok_or(LeverageDelegateError::MathOverflow)?
        } else {
            0
        };
        // Do not spend a positive gross amount when the mint would withhold
        // the entire transfer and credit the treasury nothing.
        if fee == 0 {
            debit = 0;
        }
        self.fee_recipient.reload()?;
        let before = self.fee_recipient.amount;
        if debit > 0 {
            transfer_checked(
                token_program_for_mint(&mint.to_account_info(), token_program, token_2022_program),
                source,
                mint.to_account_info(),
                self.fee_recipient.to_account_info(),
                authority,
                debit,
                mint.decimals,
                signer,
                remaining,
            )?;
        }
        self.fee_recipient.reload()?;
        let credited = self
            .fee_recipient
            .amount
            .checked_sub(before)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        require_gte!(credited, fee, LeverageDelegateError::InvalidTokenAccount);
        fee = credited;
        emit!(OrderProtocolFeePaid {
            order,
            owner,
            mint: mint.key(),
            value,
            fee,
            debit,
            credited
        });
        Ok(debit)
    }
}

#[event]
pub struct OrderProtocolFeePaid {
    pub order: Pubkey,
    pub owner: Pubkey,
    pub mint: Pubkey,
    pub value: u64,
    pub fee: u64,
    pub debit: u64,
    pub credited: u64,
}

#[cfg(test)]
mod tests {
    include!("../tests/instructions/fees.rs");
}
