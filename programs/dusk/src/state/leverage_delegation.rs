use anchor_lang::prelude::*;

use crate::{errors::ErrorCode, state::market::MarketAsset};

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
