use anchor_lang::prelude::*;

use super::accrue_fee_liability_with_remainder;
use crate::{constants::YIELD_GROWTH_FRACTION_MASK_Q64, errors::ErrorCode};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum YieldTokenKind {
    Ylp,
    Hlp,
}

impl YieldTokenKind {
    pub fn code(self) -> u8 {
        match self {
            Self::Ylp => 0,
            Self::Hlp => 1,
        }
    }
}

#[account]
#[derive(InitSpace)]
pub struct YieldAccount {
    pub owner: Pubkey,
    pub market: Pubkey,
    /// LP mint whose balance earns this account's revenue stream. This keeps
    /// base-hLP, quote-hLP, and yLP entitlements in disjoint PDA namespaces.
    pub lp_mint: Pubkey,
    pub asset_mint: Pubkey,
    pub token_kind: u8,
    pub recipient: Pubkey,
    pub swap_fee_checkpoint_q64: u128,
    pub interest_checkpoint_q64: u128,
    pub accrued_swap_fee_amount: u64,
    pub accrued_interest_amount: u64,
    /// Sub-token fixed-point entitlement carried across checkpoints. Keeping
    /// this remainder prevents transfer/checkpoint frequency from destroying
    /// holder yield through repeated flooring.
    pub swap_fee_remainder_q64: u64,
    pub interest_remainder_q64: u64,
    pub bump: u8,
}

impl YieldAccount {
    pub fn initialize(
        &mut self,
        owner: Pubkey,
        market: Pubkey,
        lp_mint: Pubkey,
        asset_mint: Pubkey,
        token_kind: YieldTokenKind,
        recipient: Pubkey,
        bump: u8,
    ) {
        self.owner = owner;
        self.market = market;
        self.lp_mint = lp_mint;
        self.asset_mint = asset_mint;
        self.token_kind = token_kind.code();
        self.recipient = recipient;
        self.swap_fee_remainder_q64 = 0;
        self.interest_remainder_q64 = 0;
        self.bump = bump;
    }

    pub fn assert_account(
        &self,
        owner: Pubkey,
        market: Pubkey,
        lp_mint: Pubkey,
        asset_mint: Pubkey,
        token_kind: YieldTokenKind,
    ) -> Result<()> {
        require_keys_eq!(self.owner, owner, ErrorCode::InvalidYieldAccount);
        require_keys_eq!(self.market, market, ErrorCode::InvalidYieldAccount);
        require_keys_eq!(self.lp_mint, lp_mint, ErrorCode::InvalidYieldAccount);
        require_keys_eq!(self.asset_mint, asset_mint, ErrorCode::InvalidYieldAccount);
        require_eq!(self.token_kind, token_kind.code(), ErrorCode::InvalidYieldAccount);
        Ok(())
    }

    pub fn accrue(
        &mut self,
        balance: u64,
        swap_fee_growth_index_q64: u128,
        interest_growth_index_q64: u128,
    ) -> Result<()> {
        let (swap_fee_amount, swap_fee_remainder_q64) = accrue_fee_liability_with_remainder(
            balance,
            swap_fee_growth_index_q64,
            self.swap_fee_checkpoint_q64,
            self.swap_fee_remainder_q64,
        )?;
        self.credit_interest_growth(balance, interest_growth_index_q64, self.interest_checkpoint_q64)?;
        self.accrued_swap_fee_amount = self
            .accrued_swap_fee_amount
            .checked_add(swap_fee_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.swap_fee_remainder_q64 = swap_fee_remainder_q64;
        self.swap_fee_checkpoint_q64 = swap_fee_growth_index_q64;
        self.interest_checkpoint_q64 = interest_growth_index_q64;
        Ok(())
    }

    /// Credit an interest-growth interval without changing this account's LP
    /// checkpoint namespace. hLP exits use this for the exiting vault-owned
    /// yLP shares while preserving sub-atom entitlement across split exits.
    pub fn credit_interest_growth(
        &mut self,
        balance: u64,
        growth_index_q64: u128,
        growth_checkpoint_q64: u128,
    ) -> Result<()> {
        let (interest_amount, interest_remainder_q64) = accrue_fee_liability_with_remainder(
            balance,
            growth_index_q64,
            growth_checkpoint_q64,
            self.interest_remainder_q64,
        )?;
        self.accrued_interest_amount = self
            .accrued_interest_amount
            .checked_add(interest_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.interest_remainder_q64 = interest_remainder_q64;
        Ok(())
    }

    pub fn claimable_amount(&self) -> Result<u64> {
        self.accrued_swap_fee_amount
            .checked_add(self.accrued_interest_amount)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub fn clear_claimed(&mut self) {
        self.accrued_swap_fee_amount = 0;
        self.accrued_interest_amount = 0;
    }

    /// Move whole-atom and scaled hLP cohort remainder into the final holder
    /// before a vault's supply reaches zero. Each persisted Q64 remainder is
    /// below one atom; their temporary sum can cross an atom boundary.
    pub fn credit_unallocated(
        &mut self,
        swap_fee_amount: u64,
        interest_amount: u64,
        swap_fee_remainder_scaled: u128,
        interest_remainder_scaled: u128,
    ) -> Result<()> {
        let swap_scaled = (self.swap_fee_remainder_q64 as u128)
            .checked_add(swap_fee_remainder_scaled)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let interest_scaled = (self.interest_remainder_q64 as u128)
            .checked_add(interest_remainder_scaled)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let swap_carry = u64::try_from(swap_scaled >> 64).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let interest_carry = u64::try_from(interest_scaled >> 64).map_err(|_| ErrorCode::MarketMathOverflow)?;
        self.accrued_swap_fee_amount = self
            .accrued_swap_fee_amount
            .checked_add(swap_fee_amount)
            .and_then(|value| value.checked_add(swap_carry))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.accrued_interest_amount = self
            .accrued_interest_amount
            .checked_add(interest_amount)
            .and_then(|value| value.checked_add(interest_carry))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.swap_fee_remainder_q64 =
            u64::try_from(swap_scaled & YIELD_GROWTH_FRACTION_MASK_Q64).map_err(|_| ErrorCode::MarketMathOverflow)?;
        self.interest_remainder_q64 = u64::try_from(interest_scaled & YIELD_GROWTH_FRACTION_MASK_Q64)
            .map_err(|_| ErrorCode::MarketMathOverflow)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::YIELD_GROWTH_SCALE_Q64;

    fn empty_yield_account() -> YieldAccount {
        YieldAccount {
            owner: Pubkey::default(),
            market: Pubkey::default(),
            lp_mint: Pubkey::default(),
            asset_mint: Pubkey::default(),
            token_kind: 0,
            recipient: Pubkey::default(),
            swap_fee_checkpoint_q64: 0,
            interest_checkpoint_q64: 0,
            accrued_swap_fee_amount: 0,
            accrued_interest_amount: 0,
            swap_fee_remainder_q64: 0,
            interest_remainder_q64: 0,
            bump: 0,
        }
    }

    #[test]
    fn split_exit_interest_growth_telescopes_to_single_exit() {
        let mut single_exit = empty_yield_account();
        single_exit
            .credit_interest_growth(1, YIELD_GROWTH_SCALE_Q64, 0)
            .unwrap();

        let mut split_exit = empty_yield_account();
        split_exit
            .credit_interest_growth(1, YIELD_GROWTH_SCALE_Q64 / 2, 0)
            .unwrap();
        split_exit
            .credit_interest_growth(1, YIELD_GROWTH_SCALE_Q64, YIELD_GROWTH_SCALE_Q64 / 2)
            .unwrap();

        assert_eq!(split_exit.accrued_interest_amount, single_exit.accrued_interest_amount);
        assert_eq!(split_exit.interest_remainder_q64, single_exit.interest_remainder_q64);
        assert_eq!(split_exit.accrued_interest_amount, 1);
        assert_eq!(split_exit.interest_remainder_q64, 0);
    }
}
