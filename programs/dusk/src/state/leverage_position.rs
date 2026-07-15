use anchor_lang::prelude::*;

use crate::{
    errors::ErrorCode,
    state::market::{Debt, MarketAsset},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum LeverageMarginMode {
    #[default]
    Debt = 0,
    Collateral = 1,
}

impl LeverageMarginMode {
    pub const fn code(self) -> u8 {
        self as u8
    }

    pub fn try_from_code(code: u8) -> Result<Self> {
        match code {
            0 => Ok(Self::Debt),
            1 => Ok(Self::Collateral),
            _ => err!(ErrorCode::InvalidLeverageMarginMode),
        }
    }
}

#[account]
#[derive(InitSpace)]
pub struct LeveragePosition {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub position_id: Pubkey,
    pub debt_asset: u8,
    /// Stable wire value: debt margin = 0, collateral margin = 1.
    pub margin_mode: u8,
    pub collateral_amount: u64,
    /// Initial net margin credit, denominated in `margin_asset()`.
    pub margin_amount: u64,
    /// Debt-token notional for debt margin; collateral-token notional for collateral margin.
    pub open_notional: u64,
    pub debt_principal: u128,
    pub debt_shares: u128,
    pub multiplier_bps: u64,
    pub opened_at: i64,
    pub opened_slot: u64,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct LeverageDelegation {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub position: Pubkey,
    pub debt_asset: u8,
    pub delegated_program: Pubkey,
    pub approved_actions: u32,
    pub bump: u8,
}

impl LeverageDelegation {
    pub fn initialize(
        &mut self,
        owner: Pubkey,
        market: Pubkey,
        position: Pubkey,
        debt_asset: MarketAsset,
        delegated_program: Pubkey,
        approved_actions: u32,
        bump: u8,
    ) {
        self.owner = owner;
        self.market = market;
        self.position = position;
        self.debt_asset = debt_asset.code();
        self.delegated_program = delegated_program;
        self.approved_actions = approved_actions;
        self.bump = bump;
    }

    pub fn update(&mut self, delegated_program: Pubkey, approved_actions: u32) {
        self.delegated_program = delegated_program;
        self.approved_actions = approved_actions;
    }

    pub fn assert_delegation(
        &self,
        owner: Pubkey,
        market: Pubkey,
        position: Pubkey,
        debt_asset: MarketAsset,
    ) -> Result<()> {
        require_keys_eq!(self.owner, owner, ErrorCode::InvalidLeverageDelegation);
        require_keys_eq!(self.market, market, ErrorCode::InvalidLeverageDelegation);
        require_keys_eq!(self.position, position, ErrorCode::InvalidLeverageDelegation);
        require!(self.debt_asset()? == debt_asset, ErrorCode::InvalidLeverageDelegation);
        Ok(())
    }

    pub fn debt_asset(&self) -> Result<MarketAsset> {
        MarketAsset::try_from_code(self.debt_asset)
    }
}

impl LeveragePosition {
    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        &mut self,
        owner: Pubkey,
        market: Pubkey,
        position_id: Pubkey,
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
        self.initialize_with_margin_mode(
            owner,
            market,
            position_id,
            debt_asset,
            LeverageMarginMode::Debt,
            collateral_amount,
            margin_amount,
            open_notional,
            debt_principal,
            debt_shares,
            multiplier_bps,
            opened_at,
            opened_slot,
            bump,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn initialize_with_margin_mode(
        &mut self,
        owner: Pubkey,
        market: Pubkey,
        position_id: Pubkey,
        debt_asset: MarketAsset,
        margin_mode: LeverageMarginMode,
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
        self.position_id = position_id;
        self.debt_asset = debt_asset.code();
        self.margin_mode = margin_mode.code();
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

    pub fn assert_position(&self, owner: Pubkey, market: Pubkey, debt_asset: MarketAsset) -> Result<()> {
        require_keys_eq!(self.owner, owner, ErrorCode::InvalidLeveragePosition);
        require_keys_eq!(self.market, market, ErrorCode::InvalidLeveragePosition);
        require!(self.debt_asset()? == debt_asset, ErrorCode::InvalidLeveragePosition);
        Ok(())
    }

    pub fn debt_asset(&self) -> Result<MarketAsset> {
        MarketAsset::try_from_code(self.debt_asset)
    }

    pub fn collateral_asset(&self) -> Result<MarketAsset> {
        Ok(self.debt_asset()?.opposite())
    }

    pub fn margin_mode(&self) -> Result<LeverageMarginMode> {
        LeverageMarginMode::try_from_code(self.margin_mode)
    }

    pub fn margin_asset(&self) -> Result<MarketAsset> {
        match self.margin_mode()? {
            LeverageMarginMode::Debt => self.debt_asset(),
            LeverageMarginMode::Collateral => self.collateral_asset(),
        }
    }

    pub fn settlement_asset(&self) -> Result<MarketAsset> {
        self.margin_asset()
    }

    pub fn require_margin_mode(&self, expected: LeverageMarginMode) -> Result<()> {
        require!(self.margin_mode()? == expected, ErrorCode::InvalidLeverageMarginMode);
        Ok(())
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
