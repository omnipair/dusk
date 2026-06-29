use anchor_lang::prelude::*;

use crate::{
    errors::ErrorCode,
    state::market::{Debt, MarketAsset},
};

#[account]
#[derive(InitSpace)]
pub struct LeveragePosition {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub debt_asset: u8,
    pub collateral_amount: u64,
    pub margin_amount: u64,
    pub open_notional: u64,
    pub debt_principal: u128,
    pub debt_shares: u128,
    pub multiplier_bps: u64,
    pub opened_at: i64,
    pub opened_slot: u64,
    pub bump: u8,
}

impl LeveragePosition {
    pub fn initialize(
        &mut self,
        owner: Pubkey,
        market: Pubkey,
        debt_asset: MarketAsset,
        collateral_amount: u64,
        margin_amount: u64,
        open_notional: u64,
        debt_principal: u64,
        debt_shares: u128,
        multiplier_bps: u64,
        opened_at: i64,
        opened_slot: u64,
        bump: u8,
    ) {
        self.owner = owner;
        self.market = market;
        self.debt_asset = debt_asset.code();
        self.collateral_amount = collateral_amount;
        self.margin_amount = margin_amount;
        self.open_notional = open_notional;
        self.debt_principal = debt_principal as u128;
        self.debt_shares = debt_shares;
        self.multiplier_bps = multiplier_bps;
        self.opened_at = opened_at;
        self.opened_slot = opened_slot;
        self.bump = bump;
    }

    pub fn is_initialized(&self) -> bool {
        self.owner != Pubkey::default() && self.market != Pubkey::default()
    }

    pub fn assert_position(
        &self,
        owner: Pubkey,
        market: Pubkey,
        debt_asset: MarketAsset,
    ) -> Result<()> {
        require_keys_eq!(self.owner, owner, ErrorCode::InvalidLeveragePosition);
        require_keys_eq!(self.market, market, ErrorCode::InvalidLeveragePosition);
        require!(
            self.debt_asset()? == debt_asset,
            ErrorCode::InvalidLeveragePosition
        );
        Ok(())
    }

    pub fn debt_asset(&self) -> Result<MarketAsset> {
        MarketAsset::try_from_code(self.debt_asset)
    }

    pub fn collateral_asset(&self) -> Result<MarketAsset> {
        Ok(self.debt_asset()?.opposite())
    }

    pub fn debt_amount(&self, debt: &Debt) -> Result<u64> {
        let amount = Debt::shares_to_debt(self.debt_shares, debt.borrow_index(self.debt_asset()?))?;
        u64::try_from(amount).map_err(|_| ErrorCode::DebtMathOverflow.into())
    }

    pub fn require_open(&self) -> Result<()> {
        require!(self.debt_shares > 0, ErrorCode::ZeroDebtAmount);
        require!(self.collateral_amount > 0, ErrorCode::InsufficientAmount);
        Ok(())
    }

    pub fn credit_collateral(&mut self, amount: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::AmountZero);
        self.collateral_amount = self
            .collateral_amount
            .checked_add(amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }

    pub fn debit_collateral(&mut self, amount: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::AmountZero);
        self.collateral_amount = self
            .collateral_amount
            .checked_sub(amount)
            .ok_or(ErrorCode::InsufficientAmount)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    include!("../tests/state/leverage_position.rs");
}
