use crate::{
    errors::ErrorCode,
    state::{Market, MarketAsset},
    transitions::AddLiquidityReceipt,
};
use anchor_lang::prelude::*;

impl Market {
    pub fn fund_junior_seed(&mut self, asset: MarketAsset, debit: u64, credit: u64) -> Result<()> {
        require_gte!(debit, credit, ErrorCode::BrokenInvariant);
        let fees = &mut self.side_mut(asset).fees;
        fees.junior.seed_interest_amount = fees
            .junior
            .seed_interest_amount
            .checked_sub(debit)
            .ok_or(ErrorCode::UnbackedFeeLiability)?;
        fees.interest_liability = fees
            .interest_liability
            .checked_sub(debit)
            .ok_or(ErrorCode::UnbackedFeeLiability)?;
        fees.interest_vault_balance = fees
            .interest_vault_balance
            .checked_sub(debit)
            .ok_or(ErrorCode::UnbackedFeeLiability)?;
        fees.junior.seed_swap_amount = fees
            .junior
            .seed_swap_amount
            .checked_add(credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        fees.swap_fee_liability = fees
            .swap_fee_liability
            .checked_add(credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        fees.swap_fee_custody_balance = fees
            .swap_fee_custody_balance
            .checked_add(credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        fees.assert_backed()
    }

    /// Reinvest a balanced slice without granting any senior tokens. The
    /// unused asset stays a seed-only custody claim until its pair is available.
    pub fn compound_junior_seed(&mut self) -> Result<AddLiquidityReceipt> {
        require!(self.base_side.shares.ylp_supply > 0, ErrorCode::InsufficientLiquidity);
        require!(
            self.tranches.junior_reserve_shares > 0 || self.tranches.junior_receipt_supply == 0,
            ErrorCode::InvalidArgument
        );
        self.checkpoint_junior_seed()?;
        let receipt = self.preview_add_liquidity(
            self.base_side.fees.junior.seed_swap_amount,
            self.quote_side.fees.junior.seed_swap_amount,
        )?;
        for (asset, amount) in [
            (MarketAsset::Base, receipt.base_reserve_credit),
            (MarketAsset::Quote, receipt.quote_reserve_credit),
        ] {
            let fees = &mut self.side_mut(asset).fees;
            fees.junior.seed_swap_amount = fees
                .junior
                .seed_swap_amount
                .checked_sub(amount)
                .ok_or(ErrorCode::UnbackedFeeLiability)?;
            fees.swap_fee_liability = fees
                .swap_fee_liability
                .checked_sub(amount)
                .ok_or(ErrorCode::UnbackedFeeLiability)?;
            fees.swap_fee_custody_balance = fees
                .swap_fee_custody_balance
                .checked_sub(amount)
                .ok_or(ErrorCode::UnbackedFeeLiability)?;
        }
        let funded = self.add_liquidity(receipt.base_reserve_credit, receipt.quote_reserve_credit)?;
        require_eq!(funded.ylp_amount, receipt.ylp_amount, ErrorCode::BrokenInvariant);
        let issued = self.tranches.enter(funded.ylp_amount, funded.ylp_supply)?;
        self.tranches.permanent_receipts = self
            .tranches
            .permanent_receipts
            .checked_add(issued)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.synchronize_tranche_income();
        Ok(funded)
    }
}
