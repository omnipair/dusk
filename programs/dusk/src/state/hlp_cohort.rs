use anchor_lang::prelude::*;

use crate::{
    constants::HLP_COHORT_SEED_PREFIX,
    errors::ErrorCode,
    state::{HlpVault, MarketAsset},
};

/// The mint-specific lifetime of one hLP receipt cohort. While current, the
/// market vault owns the live accounting. After terminal recovery, this account
/// holds the final yield indexes and supply so old receipts can be transferred,
/// harvested and burned without acquiring backing from a new deposit cohort.
#[account]
#[derive(InitSpace, Default)]
pub struct HlpCohort {
    pub market: Pubkey,
    pub mint: Pubkey,
    pub target_asset: u8,
    pub retired: bool,
    pub final_vault: HlpVault,
    pub bump: u8,
}

impl HlpCohort {
    #[inline(never)]
    pub fn from_remaining(info: &AccountInfo, market: Pubkey, mint: Pubkey) -> Result<Self> {
        require_keys_eq!(*info.owner, crate::ID, ErrorCode::InvalidArgument);
        let data = info.try_borrow_data()?;
        let mut data_slice: &[u8] = &data;
        let cohort = Self::try_deserialize(&mut data_slice)?;
        cohort.assert_retired(market, mint, *info.key)?;
        Ok(cohort)
    }

    pub fn assert_identity(&self, market: Pubkey, mint: Pubkey, target_asset: MarketAsset) -> Result<()> {
        require_keys_eq!(self.market, market, ErrorCode::InvalidMarket);
        require_keys_eq!(self.mint, mint, ErrorCode::InvalidLpMintKey);
        require_eq!(self.target_asset, target_asset.code(), ErrorCode::InvalidLpMintKey);
        Ok(())
    }

    pub fn assert_retired(&self, market: Pubkey, mint: Pubkey, account: Pubkey) -> Result<MarketAsset> {
        let target_asset = MarketAsset::try_from_code(self.target_asset)?;
        self.assert_identity(market, mint, target_asset)?;
        let (expected, bump) =
            Pubkey::find_program_address(&[HLP_COHORT_SEED_PREFIX, market.as_ref(), mint.as_ref()], &crate::ID);
        require_keys_eq!(account, expected, ErrorCode::InvalidArgument);
        require_eq!(self.bump, bump, ErrorCode::InvalidArgument);
        require!(self.retired, ErrorCode::InvalidArgument);
        Ok(target_asset)
    }

    pub fn yield_indexes(&self, asset: MarketAsset) -> (u128, u128) {
        match asset {
            MarketAsset::Base => (
                self.final_vault.base_swap_fee_growth_index_q64,
                self.final_vault.base_interest_growth_index_q64,
            ),
            MarketAsset::Quote => (
                self.final_vault.quote_swap_fee_growth_index_q64,
                self.final_vault.quote_interest_growth_index_q64,
            ),
        }
    }
}
