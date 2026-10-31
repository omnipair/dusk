use crate::{
    errors::ErrorCode,
    state::{MarketAsset, TrancheIncome},
};
use anchor_lang::prelude::*;

#[account]
#[derive(InitSpace, Default)]
pub struct JuniorCohort {
    pub income_end: [crate::state::IncomeExposureEnd; 4],
    pub market: Pubkey,
    pub mint: Pubkey,
    pub generation: u64,
    pub retired: bool,
    pub final_income: [TrancheIncome; 2],
    pub bump: u8,
}

impl JuniorCohort {
    pub fn from_remaining(info: &AccountInfo, market: Pubkey, mint: Pubkey) -> Result<Self> {
        require_keys_eq!(*info.owner, crate::ID, ErrorCode::InvalidArgument);
        let data = info.try_borrow_data()?;
        let cohort = Self::try_deserialize(&mut &data[..])?;
        require_keys_eq!(cohort.market, market, ErrorCode::InvalidMarket);
        require_keys_eq!(cohort.mint, mint, ErrorCode::InvalidLpMintKey);
        require!(cohort.retired, ErrorCode::InvalidArgument);
        let (key, bump) = Pubkey::find_program_address(&[b"junior_cohort", market.as_ref(), mint.as_ref()], &crate::ID);
        require_keys_eq!(*info.key, key, ErrorCode::InvalidArgument);
        require_eq!(cohort.bump, bump, ErrorCode::InvalidArgument);
        Ok(cohort)
    }

    pub fn yield_indexes(&self, asset: MarketAsset) -> (u128, u128) {
        let income = self.final_income[asset.code() as usize];
        (income.swap_growth_q64, income.interest_growth_q64)
    }
}
