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
    pub open_curve_revision: u64,
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
        open_curve_revision: u64,
    ) -> Result<()> {
        self.assert_identity(owner, market, position, debt_asset)?;
        require_eq!(
            self.open_curve_revision,
            open_curve_revision,
            ErrorCode::InvalidLeverageDelegation
        );
        Ok(())
    }

    pub fn assert_identity(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recreated_position_invalidates_prior_delegation_until_owner_updates_it() {
        let owner = Pubkey::new_unique();
        let market = Pubkey::new_unique();
        let position = Pubkey::new_unique();
        let mut delegation = LeverageDelegation {
            owner,
            market,
            position,
            debt_asset: MarketAsset::Base.code(),
            delegated_program: Pubkey::new_unique(),
            approved_actions: 1,
            open_curve_revision: 10,
            bump: 1,
        };
        assert!(delegation
            .assert_delegation(owner, market, position, MarketAsset::Base, 10)
            .is_ok());
        assert!(delegation
            .assert_delegation(owner, market, position, MarketAsset::Base, 11)
            .is_err());
        delegation.open_curve_revision = 11;
        assert!(delegation
            .assert_delegation(owner, market, position, MarketAsset::Base, 11)
            .is_ok());
    }
}
