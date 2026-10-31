use crate::{
    constants::NAD,
    errors::ErrorCode,
    math::{borrow_controller::coverage_signal_nad, mul_div_u128, utilization_bps},
    state::{Debt, Market, MarketAsset},
};
use anchor_lang::prelude::*;

impl Market {
    /// Direct Token-2022 burns donate future ownership to the permanent seed.
    /// They cannot recreate the sender's abandoned, uncheckpointed income.
    pub fn reconcile_junior_receipts(&mut self, live_mint_supply: u64) -> Result<()> {
        let expected = self
            .tranches
            .junior_receipt_supply
            .checked_sub(self.tranches.permanent_receipts)
            .and_then(|v| v.checked_sub(self.tranches.queued_receipts))
            .ok_or(ErrorCode::BrokenInvariant)?;
        require_gte!(expected, live_mint_supply, ErrorCode::InvalidArgument);
        if expected != live_mint_supply {
            self.checkpoint_junior_seed()?;
            self.tranches.permanent_receipts = self
                .tranches
                .permanent_receipts
                .checked_add(expected - live_mint_supply)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        }
        Ok(())
    }

    pub fn checkpoint_junior_seed(&mut self) -> Result<()> {
        self.base_side.fees.junior.checkpoint_seed(&self.tranches)?;
        self.quote_side.fees.junior.checkpoint_seed(&self.tranches)?;
        self.checkpoint_seed_receivables()?;
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let i = asset.code() as usize;
            let amount = self.income.get()?.seed[i]
                .claimable
                .checked_add(self.income.get()?.seed[i + 2].claimable)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            self.income.get_mut()?.seed[i].claimable = 0;
            self.income.get_mut()?.seed[i + 2].claimable = 0;
            let seed = &mut self.side_mut(asset).fees.junior.seed_interest_amount;
            *seed = seed.checked_add(amount).ok_or(ErrorCode::MarketMathOverflow)?;
        }
        Ok(())
    }

    pub fn synchronize_tranche_income(&mut self) {
        self.base_side.fees.junior.synchronize(&self.tranches);
        self.quote_side.fees.junior.synchronize(&self.tranches);
    }

    /// The reserve/debt settlement has already removed this funded loss.
    /// Burn junior's common reserve units to preserve senior units and value.
    pub fn absorb_funded_credit_loss(&mut self, asset: MarketAsset, loss: u64) -> Result<()> {
        if loss == 0 || self.tranches.junior_reserve_shares == 0 {
            return Ok(());
        }
        self.checkpoint_junior_seed()?;
        let normalized = self.normalize_amount(loss as u128, self.side(asset).asset_decimals)?;
        let mark = if self.amm.price_ema_nad > 0 {
            self.amm.price_ema_nad
        } else if self.risk.base_price_ema_nad > 0 {
            self.risk.base_price_ema_nad
        } else {
            self.amm.launch_reference_price_nad
        };
        let value = if asset == MarketAsset::Base {
            mul_div_u128(normalized, mark as u128, NAD as u128)?
        } else {
            normalized
        };
        let before = self
            .funded_reserve_value()?
            .checked_add(value)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let mut supply = self.base_side.shares.ylp_supply;
        let loss = self.tranches.absorb_loss(before, value, &mut supply)?;
        if loss.junior_wiped {
            for i in 0..4 {
                self.income.get_mut()?.junior_end[i] = crate::state::IncomeExposureEnd {
                    ended: true,
                    epoch: self.income.get()?.books[i].epoch,
                    growth: self.income.get()?.books[i].growth[1],
                };
            }
        }
        self.base_side.shares.ylp_supply = supply;
        self.quote_side.shares.ylp_supply = supply;
        self.synchronize_tranche_income();
        Ok(())
    }

    /// Maximum ordinary cash draw under the configured utilization constraint,
    /// including the aggregate rounding caused by minting borrower debt shares.
    pub fn borrow_cash_headroom(&self, asset: MarketAsset) -> Result<u64> {
        let cash = self.side(asset).reserves.cash_reserve;
        let debt = self.covered_debt(asset)?;
        let limit = self.config.irm.borrow_utilization_limit_bps as u128;
        let bound = mul_div_u128(
            debt.checked_add(cash as u128).ok_or(ErrorCode::MarketMathOverflow)?,
            limit,
            10_000,
        )?
        .saturating_sub(debt);
        let mut low = 0;
        let mut high = cash.min(u64::try_from(bound).unwrap_or(u64::MAX));
        while low < high {
            let amount = low + (high - low) / 2 + (high - low) % 2;
            let shares = Debt::debt_to_shares(amount, self.debt.borrow_index(asset))?;
            let increase = self.debt.fixed_debt_increase_for_shares(asset, shares)?;
            if crate::math::borrow_controller::check_new_borrow_liquidity(
                &self.config.irm,
                debt,
                cash,
                increase as u128,
                amount,
            )
            .is_ok()
            {
                low = amount;
            } else {
                high = amount - 1;
            }
        }
        Ok(low)
    }
    /// Consolidated borrower exposure, including opposite-vault hLP funding.
    pub fn covered_debt(&self, asset: MarketAsset) -> Result<u128> {
        let public = match asset {
            MarketAsset::Base => self.debt.fixed_base_debt()?,
            MarketAsset::Quote => self.debt.fixed_quote_debt()?,
        }
        .checked_add(self.debt.isolated_debt(asset)?)
        .ok_or(ErrorCode::MarketMathOverflow)?;
        let hlp = match asset {
            MarketAsset::Base => self.quote_hlp_vault.debt_shares,
            MarketAsset::Quote => self.base_hlp_vault.debt_shares,
        };
        public
            .checked_add(Debt::shares_to_debt(hlp, self.debt.borrow_index(asset))?)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    /// Quote-denominated funded reserve value. An unfunded public-interest
    /// receivable cannot increase underwriting capital. Cash-funded advances
    /// remain reserve credit exposure until repayment/writeoff clears them.
    pub fn funded_reserve_value(&self) -> Result<u128> {
        let mark = if self.amm.price_ema_nad > 0 {
            self.amm.price_ema_nad
        } else {
            self.risk.base_price_ema_nad
        };
        let mark = if mark == 0 {
            self.amm.launch_reference_price_nad
        } else {
            mark
        };
        require!(mark > 0, ErrorCode::InvalidSettlementPrice);
        let mut values = [0_u128; 2];
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let side = self.side(asset);
            let unfunded = self
                .public_unpaid_interest(asset)?
                .checked_sub(side.fees.public_interest_advance.outstanding_gross as u128)
                .ok_or(ErrorCode::BrokenInvariant)?;
            let funded = (side.reserves.live_reserve as u128)
                .checked_sub(unfunded)
                .ok_or(ErrorCode::BrokenInvariant)?;
            values[asset.code() as usize] = self.normalize_amount(funded, side.asset_decimals)?;
        }
        mul_div_u128(values[0], mark as u128, NAD as u128)?
            .checked_add(values[1])
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    /// Freeze both utilization inputs and the single junior/debt coverage factor
    /// before either debt index changes. No debt on an asset holds its APR.
    pub fn tranche_coverage_signals(&self) -> Result<[Option<u128>; 2]> {
        let debts = [
            self.covered_debt(MarketAsset::Base)?,
            self.covered_debt(MarketAsset::Quote)?,
        ];
        if self.tranches.junior_reserve_shares == 0 {
            return Ok(debts.map(|debt| if debt == 0 { None } else { Some(0) }));
        }
        if debts == [0, 0] {
            return Ok([None, None]);
        }
        let mark = if self.amm.price_ema_nad > 0 {
            self.amm.price_ema_nad
        } else {
            self.risk.base_price_ema_nad
        };
        let mark = if mark == 0 {
            self.amm.launch_reference_price_nad
        } else {
            mark
        };
        require!(mark > 0, ErrorCode::InvalidSettlementPrice);
        let base_debt = self.normalize_amount(debts[0], self.base_side.asset_decimals)?;
        let quote_debt = self.normalize_amount(debts[1], self.quote_side.asset_decimals)?;
        let total_debt_value = mul_div_u128(base_debt, mark as u128, NAD as u128)?
            .checked_add(quote_debt)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let junior_value = self
            .tranches
            .junior_value(self.funded_reserve_value()?, self.base_side.shares.ylp_supply)?;
        let mut signals = [None, None];
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let i = asset.code() as usize;
            if debts[i] > 0 {
                signals[i] = coverage_signal_nad(
                    junior_value,
                    total_debt_value,
                    utilization_bps(debts[i], self.side(asset).reserves.cash_reserve as u128)?,
                )?;
            }
        }
        Ok(signals)
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/transitions/tranches/market.rs");
}
