use anchor_lang::prelude::*;

use crate::errors::ErrorCode;
use crate::state::market::{Debt, MarketAsset};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CollateralReceipt {
    pub collateral_credit: u64,
    pub collateral_debit: u64,
    pub base_collateral: u64,
    pub quote_collateral: u64,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub base_liquidation_cf_bps: u16,
    pub quote_liquidation_cf_bps: u16,
}

#[account]
#[derive(InitSpace)]
pub struct BorrowPosition {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub position_id: Pubkey,
    pub base_collateral: u64,
    pub quote_collateral: u64,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub base_liquidation_cf_bps: u16,
    pub quote_liquidation_cf_bps: u16,
    pub fixed_base_shares: u128,
    pub fixed_quote_shares: u128,
    pub auction_start_time: i64,
    pub auction_start_price_nad: u64,
    pub auction_floor_price_nad: u64,
    pub bump: u8,
}

impl BorrowPosition {
    pub fn initialize(&mut self, owner: Pubkey, market: Pubkey, position_id: Pubkey, bump: u8) {
        self.owner = owner;
        self.market = market;
        self.position_id = position_id;
        self.auction_start_time = 0;
        self.auction_start_price_nad = 0;
        self.auction_floor_price_nad = 0;
        self.base_liquidation_cf_bps = 0;
        self.quote_liquidation_cf_bps = 0;
        self.bump = bump;
    }

    pub fn is_initialized(&self) -> bool {
        self.owner != Pubkey::default() && self.market != Pubkey::default()
    }

    pub fn assert_position(&self, owner: Pubkey, market: Pubkey) -> Result<()> {
        require_keys_eq!(self.owner, owner, ErrorCode::InvalidPositionMarket);
        require_keys_eq!(self.market, market, ErrorCode::InvalidPositionMarket);
        Ok(())
    }

    pub fn fixed_base_debt(&self, debt: &Debt) -> Result<u128> {
        Debt::shares_to_debt(self.fixed_base_shares, debt.base_borrow_index_nad)
    }

    pub fn fixed_quote_debt(&self, debt: &Debt) -> Result<u128> {
        Debt::shares_to_debt(self.fixed_quote_shares, debt.quote_borrow_index_nad)
    }

    pub fn liquidation_cf_bps(&self, debt_asset: MarketAsset) -> u16 {
        match debt_asset {
            MarketAsset::Base => self.base_liquidation_cf_bps,
            MarketAsset::Quote => self.quote_liquidation_cf_bps,
        }
    }

    pub fn set_liquidation_cf_bps(&mut self, debt_asset: MarketAsset, liquidation_cf_bps: u16) {
        match debt_asset {
            MarketAsset::Base => self.base_liquidation_cf_bps = liquidation_cf_bps,
            MarketAsset::Quote => self.quote_liquidation_cf_bps = liquidation_cf_bps,
        }
    }

    pub fn global_health_contribution(&self, debt_asset: MarketAsset) -> u64 {
        match debt_asset {
            MarketAsset::Base => self.global_health_quote_contribution_for_base_debt,
            MarketAsset::Quote => self.global_health_base_contribution_for_quote_debt,
        }
    }

    pub fn collateral(&self, asset: MarketAsset) -> u64 {
        match asset {
            MarketAsset::Base => self.base_collateral,
            MarketAsset::Quote => self.quote_collateral,
        }
    }
}
