use crate::{
    errors::ErrorCode,
    state::{Market, MarketAsset},
    transitions::AddLiquidityReceipt,
};
use anchor_lang::prelude::*;

/// Seed reinvestment is permissionless, so it cannot accept a caller-selected
/// slippage bound. Keep the executable AMM ratio within one basis point of its
/// time-weighted center before turning accumulated custody into LP principal.
const SEED_COMPOUND_MAX_PRICE_DISTANCE_NAD: u128 = crate::constants::NAD as u128 / 10_000;

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
        if self.amm.initialized {
            let spot = self
                .current_concentrated_spot_price_nad()?
                .ok_or(ErrorCode::InvalidSettlementPrice)?;
            let reference = self.amm.price_ema_nad;
            require!(reference > 0, ErrorCode::InvalidSettlementPrice);
            let high = u128::from(spot.max(reference));
            let low = u128::from(spot.min(reference));
            let distance = crate::math::ceil_div(
                high.checked_mul(crate::constants::NAD as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                low,
            )
            .and_then(|ratio| ratio.checked_sub(crate::constants::NAD as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
            require!(
                distance <= SEED_COMPOUND_MAX_PRICE_DISTANCE_NAD,
                ErrorCode::SlippageExceeded
            );
        }
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
