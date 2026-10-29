use super::*;
use crate::transitions::{liquidation_policy::LiquidationLossAllocation, LeverageCollateralFee};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct HlpFlashTerminalSettlement {
    pub base: HlpRebalanceReceipt,
    pub quote: HlpRebalanceReceipt,
    pub loss: LiquidationLossAllocation,
    pub insurance_debit: u64,
    pub surplus_debit: u64,
    pub surplus_credit: u64,
}

impl Market {
    /// Price and apply the terminal ledger atomically. The instruction must
    /// transfer exactly the returned insurance, interest and surplus amounts.
    /// Caller slippage caps can reject but cannot skip available insurance.
    pub(crate) fn settle_terminal_hlp_flash(
        &mut self,
        quote: HlpFlashQuote,
        debt_fee: LeverageCollateralFee,
        max_insurance_draw: u64,
        max_socialized_loss: u64,
        slot: u64,
    ) -> Result<HlpFlashTerminalSettlement> {
        require!(quote.full, ErrorCode::InvalidLiquidationSession);
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
        let debt = u64::try_from(Debt::shares_to_debt(
            vault.debt_shares,
            self.debt.borrow_index(borrowed),
        )?)
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        let collateral_value =
            self.hlp_ema_collateral_value_in_borrowed(target, quote.collateral_debit, quote.borrowed_claim, slot)?;
        require!(collateral_value <= debt, ErrorCode::HlpNotLiquidatable);
        let recovery = quote
            .borrowed_claim
            .checked_add(quote.reserve_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let capacity = self.insurance.draw_capacity(borrowed, slot)?;
        let mut loss = LiquidationLossAllocation::quote_with_coverage(
            vault.debt_principal.min(debt),
            debt,
            recovery,
            debt_fee.unwind_credit(capacity)?,
            self.insurance.principal_coverage_bps,
        )?;
        let insurance_debit = debt_fee.effective_gross_for_credit(loss.insurance_credit)?;
        require_gte!(max_insurance_draw, insurance_debit, ErrorCode::InsuranceDrawExceeded);
        require_gte!(
            max_socialized_loss,
            loss.principal_written_off,
            ErrorCode::LiquidationSocializationExceeded
        );
        let cash = self
            .side(borrowed)
            .reserves
            .cash_reserve
            .checked_add(quote.reserve_credit)
            .and_then(|v| v.checked_add(loss.insurance_credit))
            .ok_or(ErrorCode::ReserveOverflow)?;
        require_gte!(cash, loss.surplus, ErrorCode::InsufficientLiquidity);
        // Funding interest is paid from available cash. Cancel the unpaid
        // remainder; its corresponding backing stays with the pool on close.
        let cash_interest = loss.interest_paid.min(cash - loss.surplus);
        loss.interest_canceled = loss
            .interest_canceled
            .checked_add(loss.interest_paid - cash_interest)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        loss.interest_paid = cash_interest;
        self.checkpoint_hlp_yield_from_ylp(MarketAsset::Base)?;
        self.checkpoint_hlp_yield_from_ylp(MarketAsset::Quote)?;
        let initial_supply = self.base_side.shares.ylp_supply;
        self.side_mut(target).reserves.flash_receivable = self
            .side(target)
            .reserves
            .flash_receivable
            .checked_sub(quote.collateral_debit)
            .ok_or(ErrorCode::BrokenInvariant)?;
        self.side_mut(target).reserves.live_reserve = self
            .side(target)
            .reserves
            .live_reserve
            .checked_sub(quote.collateral_debit)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        self.side_mut(borrowed).credit_reserve(quote.reserve_credit, true)?;
        if insurance_debit > 0 {
            self.insurance.consume_draw(borrowed, insurance_debit, slot)?;
        }
        if loss.insurance_credit > 0 {
            self.side_mut(borrowed).credit_reserve(loss.insurance_credit, true)?;
        }
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            self.side_mut(asset).reserves.live_reserve = self
                .side(asset)
                .reserves
                .live_reserve
                .checked_sub(vault.hlp_live_reserve(asset))
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
        let payout = loss
            .interest_paid
            .checked_add(loss.surplus)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.side_mut(borrowed).reserves.cash_reserve = self
            .side(borrowed)
            .reserves
            .cash_reserve
            .checked_sub(payout)
            .ok_or(ErrorCode::InsufficientLiquidity)?;
        self.side_mut(borrowed).reserves.live_reserve = self
            .side(borrowed)
            .reserves
            .live_reserve
            .checked_sub(payout)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        self.base_side.shares.burn(vault.ylp_shares)?;
        self.quote_side.shares.burn(vault.ylp_shares)?;
        let v = match target {
            MarketAsset::Base => &mut self.base_hlp_vault,
            MarketAsset::Quote => &mut self.quote_hlp_vault,
        };
        v.ylp_shares = 0;
        v.debt_shares = 0;
        v.debt_principal = 0;
        v.base_hlp_live_reserve = 0;
        v.quote_hlp_live_reserve = 0;
        v.flash_collateral_debit = 0;
        v.residual_exposure = 0;
        v.last_nav_nad = 0;
        v.cached_settlement_price_nad = 0;
        v.funding_apr_ema_nad = 0;
        v.funding_apr_ema_last_slot = 0;
        let surplus_credit = debt_fee.unwind_credit(loss.surplus)?;
        v.publish_new_interest(borrowed, surplus_credit)?;
        let fees = &mut self.side_mut(borrowed).fees;
        fees.interest_vault_balance = fees
            .interest_vault_balance
            .checked_add(surplus_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        fees.interest_liability = fees
            .interest_liability
            .checked_add(surplus_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require!(self.hlp_frozen(target), ErrorCode::BrokenInvariant);
        self.clear_frozen_hlp(target);
        self.rebase_concentrated_curve_after_terminal_hlp_loss()?;
        // A second vault may also be insolvent. Closing this cohort must not
        // require rehedging that other cohort first; its debt and full-value
        // backing remain explicit and can be recovered independently.
        let mut base = HlpRebalanceReceipt::default();
        let mut other_quote = HlpRebalanceReceipt {
            target_asset: MarketAsset::Quote,
            ..Default::default()
        };
        let target_receipt = HlpRebalanceReceipt {
            target_asset: target,
            ylp_burn_amount: vault.ylp_shares,
            debt_delta: -i128::from(debt),
            interest_paid: loss.interest_paid,
            current_swap_fee_eligible_ylp_shares: vault.ylp_shares,
            tracking_start_ylp_shares: vault.ylp_shares,
            tracking_start_ylp_supply: initial_supply,
            ..HlpRebalanceReceipt::default()
        };
        if target == MarketAsset::Base {
            base = target_receipt;
        } else {
            other_quote = target_receipt;
        }
        self.finalize_amm_transition_and_observe_risk(slot)?;
        self.assert_market_invariants()?;
        Ok(HlpFlashTerminalSettlement {
            base,
            quote: other_quote,
            loss,
            insurance_debit,
            surplus_debit: loss.surplus,
            surplus_credit,
        })
    }
}
