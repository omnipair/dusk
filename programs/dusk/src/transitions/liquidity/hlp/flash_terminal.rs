use super::*;
use crate::state::LiquidationLossAllocation;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct HlpFlashTerminalSettlement {
    pub base: HlpRebalanceReceipt,
    pub quote: HlpRebalanceReceipt,
    pub loss: LiquidationLossAllocation,
    /// Recovery above the debt, credited to the retired cohort's holders.
    pub surplus: u64,
}

impl Market {
    /// Price and apply the terminal ledger atomically. Recovery pays
    /// principal first, then funding interest; canceled interest is a pool
    /// loss. Interest and surplus stay in reserve cash. Caller slippage caps
    /// bound the resulting loss and actual token credits.
    pub(crate) fn settle_terminal_hlp_flash(
        &mut self,
        quote: HlpFlashQuote,
        max_socialized_loss: u64,
        slot: u64,
    ) -> Result<HlpFlashTerminalSettlement> {
        require!(quote.full, ErrorCode::InvalidLiquidationSession);
        let inventory_before = self.ordinary_inventory_point_nad()?;
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
        // With an interest deficit any vault may be wound down; otherwise
        // only an insolvent one closes.
        let wind_down = self.curve_wiped();
        let collateral_value =
            self.hlp_ema_collateral_value_in_borrowed(target, quote.collateral_debit, quote.borrowed_claim, slot)?;
        require!(wind_down || collateral_value <= debt, ErrorCode::HlpNotLiquidatable);
        // Interest the frozen vault held for its creditors covers what the
        // payment leaves unrecovered; the rest goes to its holders.
        let paid_recovery = quote
            .borrowed_claim
            .checked_add(quote.reserve_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let held_use = self.plan_held_hlp_interest(target, debt.saturating_sub(paid_recovery), slot)?;
        let recovery = paid_recovery
            .checked_add(held_use.value)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let loss = LiquidationLossAllocation::quote(vault.debt_principal.min(debt), debt, recovery)?;
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
            .ok_or(ErrorCode::ReserveOverflow)?;
        require_gte!(cash, loss.surplus, ErrorCode::InsufficientLiquidity);
        self.checkpoint_hlp_yield_from_srlp(MarketAsset::Base)?;
        self.checkpoint_hlp_yield_from_srlp(MarketAsset::Quote)?;
        let initial_supply = self.base_side.shares.srlp_supply;
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
            .checked_sub(u128::from(quote.collateral_debit))
            .ok_or(ErrorCode::ReserveUnderflow)?;
        self.side_mut(borrowed).credit_reserve(quote.reserve_credit, true)?;
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            self.side_mut(asset).reserves.live_reserve = self
                .side(asset)
                .reserves
                .live_reserve
                .checked_sub(u128::from(vault.hlp_live_reserve(asset)))
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
        // Funding interest and the surplus stay in reserve cash: the protocol
        // share of the interest leaves it inside the settlement as far as
        // cash allows (the rest stays LP principal, so a small buyer credit
        // into drained cash still settles), and the surplus is credited to
        // the retired cohort's holders.
        let settlement = self.settle_funding_interest(
            borrowed,
            loss.interest_paid,
            loss.interest_canceled,
            crate::transitions::ProtocolCash::IfCashAllows,
        )?;
        let fees = &mut self.side_mut(borrowed).fees;
        fees.ledger.credited = fees
            .ledger
            .credited
            .checked_add(u128::from(loss.surplus))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.base_side.shares.burn(vault.srlp_shares)?;
        self.quote_side.shares.burn(vault.srlp_shares)?;
        let v = match target {
            MarketAsset::Base => &mut self.base_hlp_vault,
            MarketAsset::Quote => &mut self.quote_hlp_vault,
        };
        v.srlp_shares = 0;
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
        v.publish_new_interest(borrowed, loss.surplus)?;
        self.absorb_funded_credit_loss(
            borrowed,
            loss.principal_written_off
                .checked_add(settlement.lp_loss)
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )?;
        require!(wind_down || self.hlp_frozen(target), ErrorCode::BrokenInvariant);
        self.settle_held_hlp_interest(target, held_use)?;
        self.clear_frozen_hlp(target)?;
        self.exclude_hlp_funding_payment_from_pricing(borrowed, loss.interest_paid)?;
        self.rebase_concentrated_curve_after_terminal_hlp_loss(inventory_before)?;
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
            srlp_burn_amount: vault.srlp_shares,
            debt_delta: -i128::from(debt),
            interest_paid: loss.interest_paid,
            protocol_paid: settlement.protocol_paid,
            current_swap_fee_eligible_srlp_shares: vault.srlp_shares,
            tracking_start_srlp_shares: vault.srlp_shares,
            tracking_start_srlp_supply: initial_supply,
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
            surplus: loss.surplus,
        })
    }
}
