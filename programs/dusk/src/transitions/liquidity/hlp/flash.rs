//! Vault-wide external recovery. The temporary collateral receivable is backed
//! by the same-transaction settlement, never counted as spendable reserve cash.
use super::*;
use crate::transitions::LeverageCollateralFee;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, Debug, InitSpace)]
pub struct HlpFlashQuote {
    pub target_asset: u8,
    pub collateral_debit: u64,
    pub collateral_credit: u64,
    /// Spendable atoms required in the session's payment vault; includes the
    /// transfer fee from that vault to the market reserve.
    pub payment: u64,
    pub reserve_credit: u64,
    pub discount_bps: u16,
    pub full: bool,
}

impl Market {
    pub(crate) fn require_hlp_flash_idle(&self, target: MarketAsset) -> Result<()> {
        let vault = match target {
            MarketAsset::Base => &self.base_hlp_vault,
            MarketAsset::Quote => &self.quote_hlp_vault,
        };
        require_eq!(vault.flash_collateral_debit, 0, ErrorCode::LiquidationSessionActive);
        Ok(())
    }

    pub(crate) fn quote_hlp_flash_recovery(
        &self,
        target: MarketAsset,
        collateral_fee: LeverageCollateralFee,
        payment_fee: LeverageCollateralFee,
    ) -> Result<HlpFlashQuote> {
        self.require_hlp_flash_idle(target)?;
        let vault = match target {
            MarketAsset::Base => self.base_hlp_vault,
            MarketAsset::Quote => self.quote_hlp_vault,
        };
        require!(
            vault.hlp_supply > 0 && vault.ylp_shares > 0,
            ErrorCode::HlpNotLiquidatable
        );
        let borrowed = target.opposite();
        let debt = u64::try_from(Debt::shares_to_debt(
            vault.debt_shares,
            self.debt.borrow_index(borrowed),
        )?)
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        let opposite = engine::ylp_live_underlying_amount(self, borrowed, vault.ylp_shares)?;
        let target_claim = engine::ylp_live_underlying_amount(self, target, vault.ylp_shares)?;
        let gap = debt.saturating_sub(opposite);
        // A solvent vault need not wait for the native recovery swap's
        // critical-stress boundary. The fixed payment repairs the funding gap
        // while the affordable-discount check below preserves positive NAV.
        require!(self.hlp_frozen(target) || gap > 0, ErrorCode::HlpNotLiquidatable);
        let prices = current_hlp_curve_prices(self)?;
        let collateral_value =
            engine::asset_value_in_target_nad_with_prices(self, prices, target, target_claim, borrowed)?;
        let opposite_value = self.normalize_amount(u128::from(opposite), self.side(borrowed).asset_decimals)?;
        let debt_value = self.normalize_amount(u128::from(debt), self.side(borrowed).asset_decimals)?;
        let full = collateral_value
            .checked_add(opposite_value)
            .ok_or(ErrorCode::MarketMathOverflow)?
            <= debt_value;
        if full {
            self.require_hlp_ema_insolvent(target)?;
        }
        // Reuse the native recovery incentive's health bands. Payment is frozen
        // against the symmetric EMA, independently of the route's execution.
        let discount = if opposite == 0 {
            recovery::HLP_RECOVERY_MAX_DISCOUNT_BPS
        } else {
            (u128::from(gap)
                * recovery::HLP_RECOVERY_SAFE_DENOMINATOR
                * u128::from(recovery::HLP_RECOVERY_MAX_DISCOUNT_BPS)
                / u128::from(opposite))
            .min(u128::from(recovery::HLP_RECOVERY_MAX_DISCOUNT_BPS)) as u16
        };
        let reference = u128::from(match target {
            MarketAsset::Base => self.risk.base_price_ema_nad,
            MarketAsset::Quote => self.risk.quote_price_ema_nad,
        });
        require!(reference > 0, ErrorCode::InvalidSettlementPrice);
        if full {
            require_gte!(
                self.side(target).reserves.cash_reserve,
                target_claim,
                ErrorCode::InsufficientLiquidity
            );
            let received = collateral_fee.unwind_credit(target_claim)?;
            let normalized = self.normalize_amount(u128::from(received), self.side(target).asset_decimals)?;
            let fair = mul_div_u128(normalized, reference, u128::from(NAD))?;
            let discounted = mul_div_u128(
                fair,
                u128::from(BPS_DENOMINATOR - discount),
                u128::from(BPS_DENOMINATOR),
            )?;
            let credit = self.denormalize_amount_ceil(discounted, self.side(borrowed).asset_decimals)?;
            require!(credit > 0, ErrorCode::AmountZero);
            return Ok(HlpFlashQuote {
                target_asset: target.code(),
                collateral_debit: target_claim,
                collateral_credit: received,
                payment: payment_fee.effective_gross_for_credit(credit)?,
                reserve_credit: credit,
                discount_bps: discount,
                full: true,
            });
        }
        let nav = engine::hlp_share_nav_nad_with_prices(self, target, prices)?;
        let interest = debt.saturating_sub(vault.debt_principal);
        // A previously frozen vault can regain full backing as its opposite
        // yLP claim grows. It still needs a positive atomic payment to resume
        // its hedge; otherwise its receipts would remain frozen forever.
        let credit = gap
            .max(interest.saturating_sub(self.side(borrowed).reserves.cash_reserve))
            .max(u64::from(self.hlp_frozen(target)));
        let payment_value = engine::asset_value_in_target_nad_with_prices(self, prices, borrowed, credit, target)?;
        let normalized_credit = self.normalize_amount(u128::from(credit), self.side(borrowed).asset_decimals)?;
        let collateral_at = |discount_bps: u16| -> Result<(u64, u64)> {
            let fair = mul_div_ceil_u128(normalized_credit, u128::from(NAD), reference)?;
            let received = mul_div_ceil_u128(
                fair,
                u128::from(BPS_DENOMINATOR),
                u128::from(BPS_DENOMINATOR - discount_bps),
            )?;
            let net = self.denormalize_amount_ceil(received, self.side(target).asset_decimals)?;
            let gross = collateral_fee.effective_gross_for_credit(net)?;
            Ok((gross, collateral_fee.unwind_credit(gross)?))
        };
        // The ramp is a ceiling. Reduce it where necessary to retain positive
        // full-value NAV after both collateral transfer fees and the discount.
        let affordable = |bps| -> Result<bool> {
            let (gross, _) = collateral_at(bps)?;
            let removed = self.normalize_amount(u128::from(gross), self.side(target).asset_decimals)?;
            Ok(gross <= target_claim
                && removed < nav.checked_add(payment_value).ok_or(ErrorCode::MarketMathOverflow)?)
        };
        require!(affordable(0)?, ErrorCode::HlpSettlementUnavailable);
        let mut low = 0_u16;
        let mut high = discount;
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if affordable(mid)? {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        let (collateral_debit, collateral_credit) = collateral_at(low)?;
        require_gte!(
            self.side(target).reserves.cash_reserve,
            collateral_debit,
            ErrorCode::InsufficientLiquidity
        );
        Ok(HlpFlashQuote {
            target_asset: target.code(),
            collateral_debit,
            collateral_credit,
            payment: payment_fee.effective_gross_for_credit(credit)?,
            reserve_credit: credit,
            discount_bps: low,
            full: false,
        })
    }

    pub(crate) fn begin_hlp_flash_recovery(&mut self, quote: HlpFlashQuote) -> Result<()> {
        let target = MarketAsset::try_from_code(quote.target_asset)?;
        self.require_hlp_flash_idle(target)?;
        require!(
            quote.collateral_debit > 0 && quote.reserve_credit > 0,
            ErrorCode::AmountZero
        );
        let reserve = &mut self.side_mut(target).reserves;
        reserve.cash_reserve = reserve
            .cash_reserve
            .checked_sub(quote.collateral_debit)
            .ok_or(ErrorCode::InsufficientLiquidity)?;
        reserve.flash_receivable = reserve
            .flash_receivable
            .checked_add(quote.collateral_debit)
            .ok_or(ErrorCode::ReserveOverflow)?;
        let vault = match target {
            MarketAsset::Base => &mut self.base_hlp_vault,
            MarketAsset::Quote => &mut self.quote_hlp_vault,
        };
        vault.flash_collateral_debit = quote.collateral_debit;
        self.assert_virtual_reserve_invariant(target)
    }

    pub(crate) fn settle_hlp_flash_recovery(
        &mut self,
        quote: HlpFlashQuote,
        clock: crate::clock::MarketClock,
    ) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
        require!(!quote.full, ErrorCode::InvalidLiquidationSession);
        let target = MarketAsset::try_from_code(quote.target_asset)?;
        let borrowed = target.opposite();
        let vault = match target {
            MarketAsset::Base => self.base_hlp_vault,
            MarketAsset::Quote => self.quote_hlp_vault,
        };
        require_eq!(
            vault.flash_collateral_debit,
            quote.collateral_debit,
            ErrorCode::InvalidLiquidationSession
        );
        self.checkpoint_hlp_yield_from_ylp(MarketAsset::Base)?;
        self.checkpoint_hlp_yield_from_ylp(MarketAsset::Quote)?;
        // A frozen vault may have recovered positive full value while its
        // hedge was paused. This funded settlement is the point where it
        // resumes: reconstruct its indexed debt and equity before applying
        // the payment, so the payment cannot disappear into the static tranche.
        if self.hlp_frozen(target) {
            self.clear_frozen_hlp(target);
        }
        // Compose with the actual current state. Swaps, other loans, and yLP
        // operations in the middle are never overwritten with a begin snapshot.
        let mut end = self.integrated_curve_state_nad()?;
        let prices = current_hlp_curve_prices(self)?;
        let payment_value =
            engine::asset_value_in_target_nad_with_prices(self, prices, borrowed, quote.reserve_credit, target)?;
        let removed = self.normalize_amount(u128::from(quote.collateral_debit), self.side(target).asset_decimals)?;
        let payment = self.normalize_amount(u128::from(quote.reserve_credit), self.side(borrowed).asset_decimals)?;
        let signed_payment = i128::try_from(payment).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let signed_value = i128::try_from(payment_value).map_err(|_| ErrorCode::MarketMathOverflow)?;
        if target == MarketAsset::Base {
            end.base_hlp_equity = end
                .base_hlp_equity
                .checked_add(payment_value)
                .and_then(|v| v.checked_sub(removed))
                .ok_or(ErrorCode::Undercollateralized)?;
            require!(end.base_hlp_equity > 0, ErrorCode::Undercollateralized);
            end.ordinary_base_book_offset = end
                .ordinary_base_book_offset
                .checked_sub(signed_value)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            end.ordinary_quote_book_offset = end
                .ordinary_quote_book_offset
                .checked_add(signed_payment)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        } else {
            end.quote_hlp_equity = end
                .quote_hlp_equity
                .checked_add(payment_value)
                .and_then(|v| v.checked_sub(removed))
                .ok_or(ErrorCode::Undercollateralized)?;
            require!(end.quote_hlp_equity > 0, ErrorCode::Undercollateralized);
            end.ordinary_quote_book_offset = end
                .ordinary_quote_book_offset
                .checked_sub(signed_value)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            end.ordinary_base_book_offset = end
                .ordinary_base_book_offset
                .checked_add(signed_payment)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        }
        let reserve = &mut self.side_mut(target).reserves;
        reserve.flash_receivable = reserve
            .flash_receivable
            .checked_sub(quote.collateral_debit)
            .ok_or(ErrorCode::BrokenInvariant)?;
        reserve.live_reserve = reserve
            .live_reserve
            .checked_sub(quote.collateral_debit)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        self.side_mut(borrowed).credit_reserve(quote.reserve_credit, true)?;
        let vault = match target {
            MarketAsset::Base => &mut self.base_hlp_vault,
            MarketAsset::Quote => &mut self.quote_hlp_vault,
        };
        vault.flash_collateral_debit = 0;
        let plan = engine::prepare_concentrated_hlp_transition_from_end(self, end, false, true, 0, 0)?;
        let receipts = plan.consume(self)?;
        self.finalize_amm_transition_and_observe_risk(clock)?;
        self.assert_market_invariants()?;
        Ok(receipts)
    }
}

#[cfg(test)]
mod tests {
    include!("../../../tests/transitions/liquidity_hlp_flash.rs");
}
