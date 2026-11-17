use anchor_lang::prelude::*;

pub const OPERATOR_REVENUE_SEED: &[u8] = b"operator_revenue";
/// A sealed recipient's credited, unclaimed operator interest per asset (base,
/// quote). Claims are paid from reserve cash as it allows.
#[account]
#[derive(InitSpace)]
pub struct OperatorRevenueAccount {
    pub market: Pubkey,
    pub recipient: Pubkey,
    pub revenue_epoch: u64,
    pub claimable: [u128; 2],
    pub sealed: bool,
    pub bump: u8,
}

impl OperatorRevenueAccount {
    pub fn validate_identity(&self, market: Pubkey, epoch: u64, key: Pubkey) -> Result<()> {
        require!(
            self.sealed && self.revenue_epoch == epoch,
            crate::errors::ErrorCode::InvalidArgument
        );
        require_keys_eq!(self.market, market, crate::errors::ErrorCode::InvalidMarket);
        let expected = Pubkey::create_program_address(
            &[
                OPERATOR_REVENUE_SEED,
                market.as_ref(),
                &epoch.to_le_bytes(),
                &[self.bump],
            ],
            &crate::ID,
        )
        .map_err(|_| error!(crate::errors::ErrorCode::InvalidArgument))?;
        require_keys_eq!(key, expected, crate::errors::ErrorCode::InvalidArgument);
        Ok(())
    }
}
