//! Atomic new-interest allocation. Historical holder growth is never debited.
use super::*;
use crate::transitions::{HlpRebalanceReceipt, LeverageCollateralFee};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct HlpNewInterestSettlement {
    /// [target hLP][revenue asset], net newly published holder yield.
    pub published: [[u64; 2]; 2],
    /// Gross interest-custody debit sent back to each reserve.
    pub reserve_debit: [[u64; 2]; 2],
    pub reserve_credit: [[u64; 2]; 2],
    pub base_rebalance: HlpRebalanceReceipt,
    pub quote_rebalance: HlpRebalanceReceipt,
}

impl HlpVault {
    /// Take only whole atoms earned by the new public-interest index interval.
    /// An old fractional entitlement is preserved; a carry formed with it is
    /// published immediately and cannot be used to repay this vault's funding.
    fn take_new_interest(&mut self, asset: MarketAsset, index: u128, shares: u64) -> Result<u64> {
        let (checkpoint, remainder) = match asset {
            MarketAsset::Base => (
                &mut self.base_interest_checkpoint_q64,
                &mut self.base_interest_remainder_q64,
            ),
            MarketAsset::Quote => (
                &mut self.quote_interest_checkpoint_q64,
                &mut self.quote_interest_remainder_q64,
            ),
        };
        let (whole, new_remainder) = accrue_fee_liability_with_remainder(shares, index, *checkpoint, 0)?;
        let combined = u128::from(*remainder) + u128::from(new_remainder);
        *remainder = combined as u64;
        *checkpoint = index;
        self.publish_new_interest(asset, (combined >> 64) as u64)?;
        Ok(whole)
    }

    pub(super) fn publish_new_interest(&mut self, asset: MarketAsset, amount: u64) -> Result<()> {
        let (unallocated, index, carry) = match asset {
            MarketAsset::Base => (
                &mut self.unallocated_base_interest_amount,
                &mut self.base_interest_growth_index_q64,
                &mut self.base_interest_growth_remainder_scaled,
            ),
            MarketAsset::Quote => (
                &mut self.unallocated_quote_interest_amount,
                &mut self.quote_interest_growth_index_q64,
                &mut self.quote_interest_growth_remainder_scaled,
            ),
        };
        credit_hlp_growth(self.hlp_supply, unallocated, index, carry, amount)
    }
}

impl Market {
    /// The caller must checkpoint historical growth before recording the new
    /// public credit, then move exactly the returned custody debits/credits and
    /// settle the returned yLP/funding receipts in the same instruction.
    pub(crate) fn settle_new_hlp_public_interest(
        &mut self,
        eligibility: HlpYieldEligibility,
        fees: [LeverageCollateralFee; 2],
        clock: crate::clock::MarketClock,
    ) -> Result<HlpNewInterestSettlement> {
        let mut receipt = HlpNewInterestSettlement::default();
        let mut fresh = [[0_u64; 2]; 2];
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let index = self.side(asset).fees.interest_growth_index_q64;
            fresh[0][asset.code() as usize] =
                self.base_hlp_vault
                    .take_new_interest(asset, index, eligibility.base_hlp_ylp_shares)?;
            fresh[1][asset.code() as usize] =
                self.quote_hlp_vault
                    .take_new_interest(asset, index, eligibility.quote_hlp_ylp_shares)?;
        }
        // Freeze both vault valuations before any custody or ownership change.
        // Reinvesting an opposite token moves the ordinary inventory at this
        // same mark; it must not reprice the second vault midway through settlement.
        let mut end = self.integrated_curve_state_nad()?;
        let prices = current_hlp_curve_prices(self)?;
        let mut gaps = [0_u128; 2];
        for target in [MarketAsset::Base, MarketAsset::Quote] {
            let t = target.code() as usize;
            let borrowed = target.opposite();
            let vault = if t == 0 {
                self.base_hlp_vault
            } else {
                self.quote_hlp_vault
            };
            let claim = engine::ylp_live_underlying_amount(self, borrowed, vault.ylp_shares)?;
            let debt = u64::try_from(Debt::shares_to_debt(
                vault.debt_shares,
                self.debt.borrow_index(borrowed),
            )?)
            .map_err(|_| ErrorCode::DebtMathOverflow)?;
            gaps[t] = engine::asset_value_in_target_nad_with_prices(
                self,
                prices,
                borrowed,
                debt.saturating_sub(claim),
                target,
            )?;
        }
        for target in [MarketAsset::Base, MarketAsset::Quote] {
            let t = target.code() as usize;
            let borrowed = target.opposite();
            let mut gap_value = gaps[t];
            // Use new opposite-token income first, then new target-token income.
            // Published historical yield and fractional carry are never spent.
            for asset in [borrowed, target] {
                let i = asset.code() as usize;
                let needed_nad = if asset == target {
                    gap_value
                } else {
                    let (num, den) = if target == MarketAsset::Base {
                        (prices.base_in_quote_nad(), u128::from(NAD))
                    } else {
                        (u128::from(NAD), prices.base_in_quote_nad())
                    };
                    mul_div_ceil_u128(gap_value, num, den)?
                };
                let needed = self.denormalize_amount_ceil(needed_nad, self.side(asset).asset_decimals)?;
                let gross = fresh[t][i].min(fees[i].effective_gross_for_credit(needed)?);
                let credit = fees[i].unwind_credit(gross)?;
                fresh[t][i] = fresh[t][i].checked_sub(gross).ok_or(ErrorCode::BrokenInvariant)?;
                let ledger = &mut self.side_mut(asset).fees;
                ledger.interest_liability = ledger
                    .interest_liability
                    .checked_sub(gross)
                    .ok_or(ErrorCode::UnbackedFeeLiability)?;
                ledger.interest_vault_balance = ledger
                    .interest_vault_balance
                    .checked_sub(gross)
                    .ok_or(ErrorCode::UnbackedFeeLiability)?;
                self.side_mut(asset).credit_reserve(credit, true)?;
                let normalized = self.normalize_amount(u128::from(credit), self.side(asset).asset_decimals)?;
                let target_value = engine::asset_value_in_target_nad_with_prices(self, prices, asset, credit, target)?;
                gap_value = gap_value.saturating_sub(target_value);
                let signed_value = i128::try_from(target_value).map_err(|_| ErrorCode::MarketMathOverflow)?;
                let signed_credit = i128::try_from(normalized).map_err(|_| ErrorCode::MarketMathOverflow)?;
                // Book inventory receives the actual asset while the hLP owns
                // its value in the target token. The ordinary tranche performs
                // the hedge conversion at the one frozen price, conserving value.
                if asset == MarketAsset::Base {
                    end.ordinary_base_book_offset = end
                        .ordinary_base_book_offset
                        .checked_add(signed_credit)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                } else {
                    end.ordinary_quote_book_offset = end
                        .ordinary_quote_book_offset
                        .checked_add(signed_credit)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                }
                if target == MarketAsset::Base {
                    end.base_hlp_equity = end
                        .base_hlp_equity
                        .checked_add(target_value)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    end.ordinary_base_book_offset = end
                        .ordinary_base_book_offset
                        .checked_sub(signed_value)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                } else {
                    end.quote_hlp_equity = end
                        .quote_hlp_equity
                        .checked_add(target_value)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    end.ordinary_quote_book_offset = end
                        .ordinary_quote_book_offset
                        .checked_sub(signed_value)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                }
                receipt.reserve_debit[t][i] = receipt.reserve_debit[t][i]
                    .checked_add(gross)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                receipt.reserve_credit[t][i] = receipt.reserve_credit[t][i]
                    .checked_add(credit)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
        }
        let plan = engine::prepare_concentrated_hlp_transition_from_end(self, end, false, true, 0, 0)?;
        let (base, quote) = plan.consume(self)?;
        receipt.base_rebalance = base;
        receipt.quote_rebalance = quote;
        self.finalize_amm_transition_and_observe_risk(clock)?;
        // The caller settles the controller's funding-interest receipts to
        // the ordinary-yLP lane separately, before completing the instruction.
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let i = asset.code() as usize;
            self.base_hlp_vault.publish_new_interest(asset, fresh[0][i])?;
            self.quote_hlp_vault.publish_new_interest(asset, fresh[1][i])?;
        }
        receipt.published = fresh;
        self.assert_market_invariants()?;
        Ok(receipt)
    }
}
