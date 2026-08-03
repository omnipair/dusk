use anchor_lang::prelude::*;

use super::{AmmSwapQuote, FeesReceipt, Market, MarketAsset, SwapFeeBreakdown};
use crate::state::ProtocolAuctionSplit;
use crate::{
    constants::{
        BPS_DENOMINATOR, LEVERAGE_INITIAL_MARGIN_BPS, LEVERAGE_MAINTENANCE_BUFFER_BPS, LEVERAGE_MAX_MULTIPLIER_BPS,
        LEVERAGE_MAX_UNWIND_IMPACT_BPS, LIQUIDATION_INCENTIVE_BPS,
    },
    errors::ErrorCode,
    math::{denormalize_from_nad_floor, normalize_to_nad},
    state::LeveragePosition,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageSwapQuote {
    pub asset_in: u8,
    pub quoted_slot: u64,
    pub amount_in: u64,
    pub amount_in_after_fee: u64,
    pub reserve_input_credit: u64,
    pub amount_out: u64,
    pub start_price_nad: u64,
    /// Invariant-preserving trade endpoint; retained principal is excluded.
    pub end_price_nad: u64,
    /// Final executable-reserve marginal price after retained principal.
    pub reserve_end_price_nad: u64,
    pub decayed_volatility_nad: u64,
    pub post_success_volatility_nad: u64,
    /// Compatibility field: nominal claimable fee debit moved from the input
    /// reserve to its fee vault. Actual Token-2022 credit is recorded
    /// separately through `LeverageSwapFeeCredit`.
    pub fee_credit: u64,
    pub fee_breakdown: SwapFeeBreakdown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageSwapFeeCredit {
    pub base: u64,
    pub distributed_surcharge: u64,
}

impl LeverageSwapFeeCredit {
    pub fn from_total_actual_credit(quote: &LeverageSwapQuote, total_credit: u64) -> Result<Self> {
        let fee = quote.fee_breakdown;
        require_gte!(fee.claimable_fee_debit, total_credit, ErrorCode::BrokenInvariant);
        if fee.claimable_fee_debit == 0 {
            require_eq!(total_credit, 0, ErrorCode::BrokenInvariant);
            return Ok(Self::default());
        }
        let base = u64::try_from(
            (total_credit as u128)
                .checked_mul(fee.base_fee_debit as u128)
                .and_then(|value| value.checked_div(fee.claimable_fee_debit as u128))
                .ok_or(ErrorCode::FeeMathOverflow)?,
        )
        .map_err(|_| ErrorCode::FeeMathOverflow)?;
        Ok(Self {
            base,
            distributed_surcharge: total_credit.checked_sub(base).ok_or(ErrorCode::FeeMathOverflow)?,
        })
    }

    fn validate_for_quote(self, quote: &LeverageSwapQuote) -> Result<()> {
        let total = self
            .base
            .checked_add(self.distributed_surcharge)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(
            self == Self::from_total_actual_credit(quote, total)?,
            ErrorCode::BrokenInvariant
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageOpenReceipt {
    pub borrowed_amount: u64,
    pub debt_amount: u64,
    pub debt_shares: u128,
    pub notional: u64,
    pub collateral_amount: u64,
    pub closeout_value: u64,
    pub equity: u64,
    pub swap: LeverageSwapQuote,
    pub fees: FeesReceipt,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageUpdateReceipt {
    pub borrowed_amount: u64,
    pub debt_delta: i64,
    pub collateral_delta: i64,
    pub debt_amount: u64,
    pub debt_shares: u128,
    pub collateral_amount: u64,
    pub closeout_value: u64,
    pub interest_paid: u64,
    pub fees: FeesReceipt,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageCloseReceipt {
    pub debt_repaid: u64,
    pub interest_paid: u64,
    pub collateral_sold: u64,
    pub closeout_value: u64,
    pub residual: u64,
    pub swap: LeverageSwapQuote,
    pub fees: FeesReceipt,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageLiquidationReceipt {
    pub debt_repaid: u64,
    pub interest_paid: u64,
    pub principal_written_off: u64,
    pub collateral_sold: u64,
    pub closeout_value: u64,
    pub liquidator_amount: u64,
    pub owner_residual: u64,
    pub swap: LeverageSwapQuote,
    pub fees: FeesReceipt,
}

impl Market {
    pub fn quote_leverage_swap(
        &self,
        asset_in: MarketAsset,
        amount_in: u64,
        current_slot: u64,
    ) -> Result<LeverageSwapQuote> {
        Ok(Self::leverage_swap_quote_from_amm(
            self.quote_amm_swap(asset_in, amount_in, current_slot)?,
            current_slot,
        ))
    }

    fn leverage_swap_quote_from_amm(quote: AmmSwapQuote, quoted_slot: u64) -> LeverageSwapQuote {
        LeverageSwapQuote {
            asset_in: quote.asset_in.code(),
            quoted_slot,
            amount_in: quote.fee.reserve_credit,
            amount_in_after_fee: quote.fee.amount_in_for_quote,
            reserve_input_credit: quote.fee.reserve_input_credit,
            amount_out: quote.amount_out,
            start_price_nad: quote.start_price_nad,
            end_price_nad: quote.end_price_nad,
            reserve_end_price_nad: quote.reserve_end_price_nad,
            decayed_volatility_nad: quote.decayed_volatility_nad,
            post_success_volatility_nad: quote.post_success_volatility_nad,
            fee_credit: quote.fee.claimable_fee_debit,
            fee_breakdown: quote.fee,
        }
    }

    fn validate_leverage_swap_quote(
        &self,
        quote: LeverageSwapQuote,
        asset_in: MarketAsset,
        current_slot: u64,
    ) -> Result<()> {
        let fee = quote.fee_breakdown;
        require!(quote.asset_in == asset_in.code(), ErrorCode::BrokenInvariant);
        require_eq!(quote.quoted_slot, current_slot, ErrorCode::BrokenInvariant);
        require!(quote.amount_in > 0 && quote.amount_out > 0, ErrorCode::BrokenInvariant);
        require_eq!(fee.reserve_credit, quote.amount_in, ErrorCode::BrokenInvariant);
        require_eq!(
            fee.amount_in_for_quote,
            quote.amount_in_after_fee,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            fee.reserve_input_credit,
            quote.reserve_input_credit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(fee.claimable_fee_debit, quote.fee_credit, ErrorCode::BrokenInvariant);
        require_eq!(
            fee.base_fee_debit
                .checked_add(fee.dynamic_surcharge_debit)
                .ok_or(ErrorCode::FeeMathOverflow)?,
            fee.total_fee_debit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            fee.retained_surcharge
                .checked_add(fee.distributed_surcharge_debit)
                .ok_or(ErrorCode::FeeMathOverflow)?,
            fee.dynamic_surcharge_debit,
            ErrorCode::BrokenInvariant
        );
        if self.amm.retain_dynamic_surcharge {
            require_eq!(
                fee.retained_surcharge,
                fee.dynamic_surcharge_debit,
                ErrorCode::BrokenInvariant
            );
            require_eq!(fee.distributed_surcharge_debit, 0, ErrorCode::BrokenInvariant);
        } else {
            require_eq!(fee.retained_surcharge, 0, ErrorCode::BrokenInvariant);
            require_eq!(
                fee.distributed_surcharge_debit,
                fee.dynamic_surcharge_debit,
                ErrorCode::BrokenInvariant
            );
        }
        require_eq!(
            fee.amount_in_for_quote
                .checked_add(fee.total_fee_debit)
                .ok_or(ErrorCode::FeeMathOverflow)?,
            fee.reserve_credit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            fee.reserve_input_credit
                .checked_add(fee.claimable_fee_debit)
                .ok_or(ErrorCode::FeeMathOverflow)?,
            fee.reserve_credit,
            ErrorCode::BrokenInvariant
        );
        Ok(())
    }

    fn leverage_amm_quote(quote: LeverageSwapQuote, asset_in: MarketAsset) -> AmmSwapQuote {
        AmmSwapQuote::new_uncertified(
            asset_in,
            quote.amount_out,
            quote.start_price_nad,
            quote.end_price_nad,
            quote.reserve_end_price_nad,
            quote.decayed_volatility_nad,
            quote.post_success_volatility_nad,
            quote.fee_breakdown,
        )
    }

    /// Concentrated leverage swaps use the same post-trade hLP safety policy as
    /// spot. `refresh_risk` must run first so the cached price belongs to the
    /// complete executable endpoint, including retained surcharge, debt
    /// rounding, and any liquidation writeoff. No hLP token movement occurs
    /// here; the exact pending exposure is left for the permissionless hLP
    /// crank, matching concentrated spot execution.
    fn defer_hlp_after_concentrated_leverage_swap(
        &mut self,
        trade_start_price_nad: u64,
        current_slot: u64,
    ) -> Result<()> {
        if !self.has_active_hlp() || self.current_curve_parameters(current_slot).is_cpmm() {
            return Ok(());
        }
        let final_price_nad = self.risk.cached_spot_base_price_nad;
        require!(final_price_nad > 0, ErrorCode::BrokenInvariant);
        self.defer_hlp_vaults_after_concentrated_swap(trade_start_price_nad, final_price_nad)?;
        Ok(())
    }

    pub fn open_leverage(
        &mut self,
        position: &mut LeveragePosition,
        owner: Pubkey,
        market: Pubkey,
        position_id: Pubkey,
        referral_partner: Pubkey,
        referral_interest_share_bps: u16,
        debt_asset: MarketAsset,
        margin_credit: u64,
        multiplier_bps: u64,
        collateral_credit: u64,
        swap: LeverageSwapQuote,
        swap_fee_credit: LeverageSwapFeeCredit,
        opened_at: i64,
        opened_slot: u64,
        bump: u8,
        manager_fee_bps: u16,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<LeverageOpenReceipt> {
        require!(margin_credit > 0, ErrorCode::AmountZero);
        require!(multiplier_bps > BPS_DENOMINATOR as u64, ErrorCode::InvalidArgument);
        require!(
            multiplier_bps <= LEVERAGE_MAX_MULTIPLIER_BPS,
            ErrorCode::LeverageMultiplierTooHigh
        );
        let borrowed_amount = leverage_debt_from_margin(margin_credit, multiplier_bps)?;
        let notional = margin_credit
            .checked_add(borrowed_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.ensure_amm_initialized(opened_slot)?;
        require_eq!(swap.amount_in, notional, ErrorCode::BrokenInvariant);
        self.validate_leverage_swap_quote(swap, debt_asset, opened_slot)?;
        swap_fee_credit.validate_for_quote(&swap)?;
        require_gte!(swap.amount_out, collateral_credit, ErrorCode::SlippageExceeded);
        require!(collateral_credit > 0, ErrorCode::InsufficientOutputAmount);

        let closeout_quote = self.post_swap_closeout_quote_with_quote(
            debt_asset,
            swap,
            debt_asset.opposite(),
            collateral_credit,
            opened_slot,
        )?;
        let pre_finalize_closeout_value = closeout_quote.amount_out;
        let post_swap_spot_price = closeout_quote.start_price_nad;
        require_initial_leverage_health(
            self,
            debt_asset.opposite(),
            collateral_credit,
            post_swap_spot_price,
            pre_finalize_closeout_value,
            borrowed_amount,
        )?;
        self.record_leverage_borrow(debt_asset, borrowed_amount)?;
        let fees = self.apply_leverage_swap(
            debt_asset,
            swap,
            swap.amount_out,
            0,
            swap_fee_credit,
            manager_fee_bps,
            protocol_fee_bps,
            protocol_auction_split,
            opened_slot,
        )?;
        let debt_shares = self.add_isolated_borrow_debt(debt_asset, borrowed_amount)?;
        position.initialize(
            owner,
            market,
            position_id,
            referral_partner,
            referral_interest_share_bps,
            debt_asset,
            collateral_credit,
            margin_credit,
            notional,
            borrowed_amount,
            debt_shares,
            multiplier_bps,
            opened_at,
            opened_slot,
            bump,
        );
        self.finalize_amm_trade_after_inventory_checkpoint(swap.start_price_nad, swap.end_price_nad, opened_slot)?;
        let closeout_value = self.require_position_initial_leverage_health(position, opened_slot)?;
        self.refresh_risk()?;
        self.defer_hlp_after_concentrated_leverage_swap(swap.start_price_nad, opened_slot)?;
        let equity = closeout_value
            .checked_sub(borrowed_amount)
            .ok_or(ErrorCode::LeverageInitialMarginTooLow)?;
        Ok(LeverageOpenReceipt {
            borrowed_amount,
            debt_amount: borrowed_amount,
            debt_shares,
            notional,
            collateral_amount: collateral_credit,
            closeout_value,
            equity,
            swap,
            fees,
        })
    }

    pub fn increase_leverage(
        &mut self,
        position: &mut LeveragePosition,
        borrowed_amount: u64,
        collateral_credit: u64,
        swap: LeverageSwapQuote,
        swap_fee_credit: LeverageSwapFeeCredit,
        manager_fee_bps: u16,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
    ) -> Result<LeverageUpdateReceipt> {
        position.require_open()?;
        require!(borrowed_amount > 0, ErrorCode::AmountZero);
        require!(collateral_credit > 0, ErrorCode::InsufficientOutputAmount);
        let debt_asset = position.debt_asset()?;
        let debt_before = position.debt_amount(&self.debt)?;
        self.ensure_amm_initialized(current_slot)?;
        require_eq!(swap.amount_in, borrowed_amount, ErrorCode::BrokenInvariant);
        self.validate_leverage_swap_quote(swap, debt_asset, current_slot)?;
        swap_fee_credit.validate_for_quote(&swap)?;
        require_gte!(swap.amount_out, collateral_credit, ErrorCode::SlippageExceeded);
        let collateral_after = position
            .collateral_amount
            .checked_add(collateral_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let debt_after = debt_before
            .checked_add(borrowed_amount)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let closeout_quote = self.post_swap_closeout_quote_with_quote(
            debt_asset,
            swap,
            debt_asset.opposite(),
            collateral_after,
            current_slot,
        )?;
        let pre_finalize_closeout_value = closeout_quote.amount_out;
        let post_swap_spot_price = closeout_quote.start_price_nad;
        require_initial_leverage_health(
            self,
            debt_asset.opposite(),
            collateral_after,
            post_swap_spot_price,
            pre_finalize_closeout_value,
            debt_after,
        )?;
        self.record_leverage_borrow(debt_asset, borrowed_amount)?;
        let fees = self.apply_leverage_swap(
            debt_asset,
            swap,
            swap.amount_out,
            0,
            swap_fee_credit,
            manager_fee_bps,
            protocol_fee_bps,
            protocol_auction_split,
            current_slot,
        )?;
        let added_shares = self.add_isolated_borrow_debt(debt_asset, borrowed_amount)?;
        position.debt_shares = position
            .debt_shares
            .checked_add(added_shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        position.debt_principal = position
            .debt_principal
            .checked_add(borrowed_amount as u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        position.credit_collateral(collateral_credit)?;
        self.finalize_amm_trade_after_inventory_checkpoint(swap.start_price_nad, swap.end_price_nad, current_slot)?;
        let closeout_value = self.require_position_initial_leverage_health(position, current_slot)?;
        self.refresh_risk()?;
        self.defer_hlp_after_concentrated_leverage_swap(swap.start_price_nad, current_slot)?;
        Ok(LeverageUpdateReceipt {
            borrowed_amount,
            debt_delta: i64::try_from(borrowed_amount).map_err(|_| ErrorCode::Overflow)?,
            collateral_delta: i64::try_from(collateral_credit).map_err(|_| ErrorCode::Overflow)?,
            debt_amount: position.debt_amount(&self.debt)?,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: 0,
            fees,
        })
    }

    pub fn decrease_leverage(
        &mut self,
        position: &mut LeveragePosition,
        collateral_debit: u64,
        min_repay_out: u64,
        swap: LeverageSwapQuote,
        swap_fee_credit: LeverageSwapFeeCredit,
        manager_fee_bps: u16,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
    ) -> Result<LeverageUpdateReceipt> {
        position.require_open()?;
        require!(collateral_debit > 0, ErrorCode::AmountZero);
        require_gt!(
            position.collateral_amount,
            collateral_debit,
            ErrorCode::InsufficientAmount
        );
        let debt_asset = position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        let debt_before = position.debt_amount(&self.debt)?;
        self.ensure_amm_initialized(current_slot)?;
        require!(
            swap.amount_in > 0 && swap.amount_in <= collateral_debit,
            ErrorCode::BrokenInvariant
        );
        self.validate_leverage_swap_quote(swap, collateral_asset, current_slot)?;
        swap_fee_credit.validate_for_quote(&swap)?;
        require_gte!(swap.amount_out, min_repay_out, ErrorCode::SlippageExceeded);
        require_gt!(debt_before, swap.amount_out, ErrorCode::InsufficientDebt);
        let collateral_after = position
            .collateral_amount
            .checked_sub(collateral_debit)
            .ok_or(ErrorCode::InsufficientAmount)?;
        let debt_after = debt_before
            .checked_sub(swap.amount_out)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let pre_finalize_closeout_value = self
            .post_swap_closeout_quote_with_quote(
                collateral_asset,
                swap,
                collateral_asset,
                collateral_after,
                current_slot,
            )?
            .amount_out;
        require_leverage_not_liquidatable(pre_finalize_closeout_value, debt_after)?;
        let clearance = self.debt.clear_isolated_debt(
            debt_asset,
            &mut position.debt_shares,
            &mut position.debt_principal,
            swap.amount_out,
        )?;
        let live_debit = clearance.live_debit_for_cash_repay()?;
        let fees = self.apply_leverage_swap(
            collateral_asset,
            swap,
            clearance.interest_paid,
            live_debit,
            swap_fee_credit,
            manager_fee_bps,
            protocol_fee_bps,
            protocol_auction_split,
            current_slot,
        )?;
        position.debit_collateral(collateral_debit)?;
        self.finalize_amm_trade_after_inventory_checkpoint(swap.start_price_nad, swap.end_price_nad, current_slot)?;
        let closeout_value = self.leverage_closeout_value(position, current_slot)?;
        require_leverage_not_liquidatable(closeout_value, clearance.remaining_debt)?;
        self.refresh_risk()?;
        self.defer_hlp_after_concentrated_leverage_swap(swap.start_price_nad, current_slot)?;
        Ok(LeverageUpdateReceipt {
            borrowed_amount: 0,
            debt_delta: -i64::try_from(clearance.debt_reduced).map_err(|_| ErrorCode::Overflow)?,
            collateral_delta: -i64::try_from(collateral_debit).map_err(|_| ErrorCode::Overflow)?,
            debt_amount: clearance.remaining_debt,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: clearance.interest_paid,
            fees,
        })
    }

    pub fn close_leverage(
        &mut self,
        position: &mut LeveragePosition,
        min_residual_out: u64,
        swap: LeverageSwapQuote,
        swap_fee_credit: LeverageSwapFeeCredit,
        manager_fee_bps: u16,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
    ) -> Result<LeverageCloseReceipt> {
        position.require_open()?;
        let debt_asset = position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        let debt_amount = position.debt_amount(&self.debt)?;
        require_gt!(debt_amount, 0, ErrorCode::ZeroDebtAmount);
        let collateral_sold = position.collateral_amount;
        self.ensure_amm_initialized(current_slot)?;
        require!(
            swap.amount_in > 0 && swap.amount_in <= collateral_sold,
            ErrorCode::BrokenInvariant
        );
        self.validate_leverage_swap_quote(swap, collateral_asset, current_slot)?;
        swap_fee_credit.validate_for_quote(&swap)?;
        require_gte!(swap.amount_out, debt_amount, ErrorCode::InsufficientAmount);
        let residual = swap
            .amount_out
            .checked_sub(debt_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(residual, min_residual_out, ErrorCode::SlippageExceeded);
        let clearance = self.debt.clear_isolated_debt(
            debt_asset,
            &mut position.debt_shares,
            &mut position.debt_principal,
            debt_amount,
        )?;
        let live_debit = clearance.live_debit_for_cash_repay()?;
        let cash_debit = residual
            .checked_add(clearance.interest_paid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let fees = self.apply_leverage_swap(
            collateral_asset,
            swap,
            cash_debit,
            live_debit,
            swap_fee_credit,
            manager_fee_bps,
            protocol_fee_bps,
            protocol_auction_split,
            current_slot,
        )?;
        position.collateral_amount = 0;
        self.finalize_amm_trade_after_inventory_checkpoint(swap.start_price_nad, swap.end_price_nad, current_slot)?;
        self.refresh_risk()?;
        self.defer_hlp_after_concentrated_leverage_swap(swap.start_price_nad, current_slot)?;
        Ok(LeverageCloseReceipt {
            debt_repaid: debt_amount,
            interest_paid: clearance.interest_paid,
            collateral_sold,
            closeout_value: swap.amount_out,
            residual,
            swap,
            fees,
        })
    }

    pub fn liquidate_leverage(
        &mut self,
        position: &mut LeveragePosition,
        swap: LeverageSwapQuote,
        swap_fee_credit: LeverageSwapFeeCredit,
        manager_fee_bps: u16,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
    ) -> Result<LeverageLiquidationReceipt> {
        position.require_open()?;
        let debt_asset = position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        let debt_amount = position.debt_amount(&self.debt)?;
        require_gt!(debt_amount, 0, ErrorCode::ZeroDebtAmount);
        let collateral_sold = position.collateral_amount;
        self.ensure_amm_initialized(current_slot)?;
        require!(
            swap.amount_in > 0 && swap.amount_in <= collateral_sold,
            ErrorCode::BrokenInvariant
        );
        self.validate_leverage_swap_quote(swap, collateral_asset, current_slot)?;
        swap_fee_credit.validate_for_quote(&swap)?;
        let margin_bps = equity_bps(swap.amount_out, debt_amount)?;
        require!(
            swap.amount_out <= debt_amount || margin_bps <= LEVERAGE_MAINTENANCE_BUFFER_BPS as u128,
            ErrorCode::LeveragePositionNotLiquidatable
        );

        let repay_credit = swap.amount_out.min(debt_amount);
        let clearance = if repay_credit > 0 {
            self.debt.clear_isolated_debt(
                debt_asset,
                &mut position.debt_shares,
                &mut position.debt_principal,
                repay_credit,
            )?
        } else {
            Default::default()
        };
        let live_debit = clearance.live_debit_for_cash_repay()?;
        let writeoff = if position.debt_shares > 0 {
            self.debt
                .writeoff_isolated_position(debt_asset, &mut position.debt_shares, &mut position.debt_principal)?
        } else {
            Default::default()
        };
        let residual = swap.amount_out.saturating_sub(debt_amount);
        let max_incentive = (debt_amount as u128)
            .checked_mul(LIQUIDATION_INCENTIVE_BPS as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)? as u64;
        let liquidator_amount = residual.min(max_incentive);
        let owner_residual = residual
            .checked_sub(liquidator_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let cash_debit = residual
            .checked_add(clearance.interest_paid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let fees = self.apply_leverage_swap(
            collateral_asset,
            swap,
            cash_debit,
            live_debit,
            swap_fee_credit,
            manager_fee_bps,
            protocol_fee_bps,
            protocol_auction_split,
            current_slot,
        )?;
        if writeoff.aggregate_debt_written_off > 0 {
            let debt_side = self.side_mut(debt_asset);
            debt_side.reserves.live_reserve = debt_side
                .reserves
                .live_reserve
                .checked_sub(writeoff.aggregate_debt_written_off)
                .ok_or(ErrorCode::ReserveUnderflow)?;
            debt_side.assert_share_backing()?;
        }
        if writeoff.principal_written_off > 0 {
            self.checkpoint_amm_socialized_loss_raw(current_slot)?;
        }
        position.collateral_amount = 0;
        self.finalize_amm_trade_after_inventory_checkpoint(swap.start_price_nad, swap.end_price_nad, current_slot)?;
        self.refresh_risk()?;
        self.defer_hlp_after_concentrated_leverage_swap(swap.start_price_nad, current_slot)?;
        Ok(LeverageLiquidationReceipt {
            debt_repaid: clearance.debt_reduced,
            interest_paid: clearance.interest_paid,
            principal_written_off: writeoff.principal_written_off,
            collateral_sold,
            closeout_value: swap.amount_out,
            liquidator_amount,
            owner_residual,
            swap,
            fees,
        })
    }

    pub fn add_leverage_margin(
        &mut self,
        position: &mut LeveragePosition,
        repay_credit: u64,
        current_slot: u64,
    ) -> Result<LeverageUpdateReceipt> {
        position.require_open()?;
        require!(repay_credit > 0, ErrorCode::AmountZero);
        let debt_asset = position.debt_asset()?;
        let debt_before = position.debt_amount(&self.debt)?;
        require_gt!(debt_before, repay_credit, ErrorCode::InsufficientDebt);
        let pre_finalize_closeout_value = self.leverage_closeout_value(position, current_slot)?;
        let debt_after = debt_before
            .checked_sub(repay_credit)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        require_leverage_not_liquidatable(pre_finalize_closeout_value, debt_after)?;
        let clearance = self.debt.clear_isolated_debt(
            debt_asset,
            &mut position.debt_shares,
            &mut position.debt_principal,
            repay_credit,
        )?;
        let principal_paid = clearance.principal_paid;
        let live_debit = clearance.live_debit_for_cash_repay()?;
        let side = self.side_mut(debt_asset);
        side.reserves.live_reserve = side
            .reserves
            .live_reserve
            .checked_sub(live_debit)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        side.reserves.cash_reserve = side
            .reserves
            .cash_reserve
            .checked_add(principal_paid)
            .ok_or(ErrorCode::ReserveOverflow)?;
        self.finalize_amm_transition(current_slot)?;
        // Adding margin only reduces debt, so it remains available as a rescue
        // path even if a legacy position's final-curve health is poor.
        let closeout_value = self.leverage_closeout_value(position, current_slot)?;
        self.refresh_risk()?;
        Ok(LeverageUpdateReceipt {
            borrowed_amount: 0,
            debt_delta: -i64::try_from(clearance.debt_reduced).map_err(|_| ErrorCode::Overflow)?,
            collateral_delta: 0,
            debt_amount: clearance.remaining_debt,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: clearance.interest_paid,
            fees: FeesReceipt::default(),
        })
    }

    pub fn remove_leverage_margin(
        &mut self,
        position: &mut LeveragePosition,
        borrow_amount: u64,
        current_slot: u64,
    ) -> Result<LeverageUpdateReceipt> {
        position.require_open()?;
        require!(borrow_amount > 0, ErrorCode::AmountZero);
        let debt_asset = position.debt_asset()?;
        let debt_before = position.debt_amount(&self.debt)?;
        let debt_after = debt_before
            .checked_add(borrow_amount)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let pre_finalize_closeout_value = self.leverage_closeout_value(position, current_slot)?;
        let spot_price_nad = self.curve_marginal_price_nad(current_slot)?;
        require_initial_leverage_health(
            self,
            position.collateral_asset()?,
            position.collateral_amount,
            spot_price_nad,
            pre_finalize_closeout_value,
            debt_after,
        )?;
        self.record_leverage_borrow(debt_asset, borrow_amount)?;
        let shares = self.add_isolated_borrow_debt(debt_asset, borrow_amount)?;
        position.debt_shares = position
            .debt_shares
            .checked_add(shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        position.debt_principal = position
            .debt_principal
            .checked_add(borrow_amount as u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        self.finalize_amm_transition(current_slot)?;
        let closeout_value = self.require_position_initial_leverage_health(position, current_slot)?;
        self.refresh_risk()?;
        Ok(LeverageUpdateReceipt {
            borrowed_amount: borrow_amount,
            debt_delta: i64::try_from(borrow_amount).map_err(|_| ErrorCode::Overflow)?,
            collateral_delta: 0,
            debt_amount: position.debt_amount(&self.debt)?,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: 0,
            fees: FeesReceipt::default(),
        })
    }

    pub fn leverage_closeout_value(&self, position: &LeveragePosition, current_slot: u64) -> Result<u64> {
        let collateral_asset = position.collateral_asset()?;
        self.quote_leverage_swap(collateral_asset, position.collateral_amount, current_slot)
            .map(|quote| quote.amount_out)
    }

    fn require_position_initial_leverage_health(&self, position: &LeveragePosition, current_slot: u64) -> Result<u64> {
        let collateral_asset = position.collateral_asset()?;
        let closeout_value = self.leverage_closeout_value(position, current_slot)?;
        let spot_price_nad = self.curve_marginal_price_nad(current_slot)?;
        require_initial_leverage_health(
            self,
            collateral_asset,
            position.collateral_amount,
            spot_price_nad,
            closeout_value,
            position.debt_amount(&self.debt)?,
        )?;
        Ok(closeout_value)
    }

    fn post_swap_closeout_quote_with_quote(
        &self,
        asset_in: MarketAsset,
        swap: LeverageSwapQuote,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        current_slot: u64,
    ) -> Result<AmmSwapQuote> {
        let first = Self::leverage_amm_quote(swap, asset_in);
        self.quote_amm_swap_after(&first, collateral_asset, collateral_amount, current_slot)
    }

    fn apply_leverage_swap(
        &mut self,
        asset_in: MarketAsset,
        swap: LeverageSwapQuote,
        cash_debit_out: u64,
        extra_live_debit_out: u64,
        swap_fee_credit: LeverageSwapFeeCredit,
        manager_fee_bps: u16,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
    ) -> Result<FeesReceipt> {
        swap_fee_credit.validate_for_quote(&swap)?;
        require_eq!(
            swap.reserve_input_credit,
            swap.amount_in_after_fee
                .checked_add(swap.fee_breakdown.retained_surcharge)
                .ok_or(ErrorCode::ReserveOverflow)?,
            ErrorCode::BrokenInvariant
        );
        {
            let (side_in, side_out) = self.swap_sides_mut(asset_in);
            side_in.reserves.live_reserve = side_in
                .reserves
                .live_reserve
                .checked_add(swap.amount_in_after_fee)
                .ok_or(ErrorCode::ReserveOverflow)?;
            side_in.reserves.cash_reserve = side_in
                .reserves
                .cash_reserve
                .checked_add(swap.amount_in_after_fee)
                .ok_or(ErrorCode::ReserveOverflow)?;
            side_out.reserves.live_reserve = side_out
                .reserves
                .live_reserve
                .checked_sub(
                    swap.amount_out
                        .checked_add(extra_live_debit_out)
                        .ok_or(ErrorCode::ReserveUnderflow)?,
                )
                .ok_or(ErrorCode::ReserveUnderflow)?;
            side_out.reserves.cash_reserve = side_out
                .reserves
                .cash_reserve
                .checked_sub(cash_debit_out)
                .ok_or(ErrorCode::CashReserveUnderflow)?;
        }

        // The actual curve trade plus debt-accounting offsets is neutral to
        // protected liquidity. Only the retained surcharge that follows may
        // increase the recenter budget.
        self.checkpoint_amm_neutral_inventory_raw(current_slot)?;
        if swap.fee_breakdown.retained_surcharge > 0 {
            let retained = swap.fee_breakdown.retained_surcharge;
            let side_in = self.side_mut(asset_in);
            side_in.reserves.live_reserve = side_in
                .reserves
                .live_reserve
                .checked_add(retained)
                .ok_or(ErrorCode::ReserveOverflow)?;
            side_in.reserves.cash_reserve = side_in
                .reserves
                .cash_reserve
                .checked_add(retained)
                .ok_or(ErrorCode::ReserveOverflow)?;
            self.checkpoint_amm_retained_surcharge_raw(current_slot)?;
        }

        let fee_eligible_ylp_supply = self.side(asset_in).shares.ylp_supply;
        let (side_in, side_out) = self.swap_sides_mut(asset_in);
        let fees = side_in.record_claimable_swap_fees(
            swap_fee_credit.base,
            swap_fee_credit.distributed_surcharge,
            manager_fee_bps,
            protocol_fee_bps,
            protocol_auction_split,
            fee_eligible_ylp_supply,
        )?;
        side_in.assert_share_backing()?;
        side_out.assert_share_backing()?;
        side_in.fees.assert_backed()?;
        Ok(fees)
    }

    fn record_leverage_borrow(&mut self, debt_asset: MarketAsset, gross_debt: u64) -> Result<()> {
        let daily_limit = self.daily_limit_for_side(debt_asset, self.config.max_daily_borrow_bps)?;
        let current_slot = self.risk.last_snapshot_slot;
        let debt_side = self.side_mut(debt_asset);
        require_gte!(
            debt_side.reserves.cash_reserve,
            gross_debt,
            ErrorCode::InsufficientBorrowHeadroom
        );
        debt_side
            .daily_limits
            .record_borrow(gross_debt, daily_limit, current_slot)?;
        debt_side.reserves.cash_reserve = debt_side
            .reserves
            .cash_reserve
            .checked_sub(gross_debt)
            .ok_or(ErrorCode::CashReserveUnderflow)?;
        Ok(())
    }

    fn add_isolated_borrow_debt(&mut self, debt_asset: MarketAsset, cash_debit: u64) -> Result<u128> {
        let aggregate_debt_before = self.debt.isolated_debt(debt_asset)?;
        let shares = self.debt.add_isolated_debt(debt_asset, cash_debit)?;
        let aggregate_debt_after = self.debt.isolated_debt(debt_asset)?;
        let aggregate_debt_increase = u64::try_from(
            aggregate_debt_after
                .checked_sub(aggregate_debt_before)
                .ok_or(ErrorCode::DebtMathOverflow)?,
        )
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        let side = self.side_mut(debt_asset);
        if aggregate_debt_increase > cash_debit {
            side.reserves.live_reserve = side
                .reserves
                .live_reserve
                .checked_add(aggregate_debt_increase - cash_debit)
                .ok_or(ErrorCode::ReserveOverflow)?;
        } else if aggregate_debt_increase < cash_debit {
            side.reserves.live_reserve = side
                .reserves
                .live_reserve
                .checked_sub(cash_debit - aggregate_debt_increase)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
        Ok(shares)
    }
}

pub(crate) fn leverage_debt_from_margin(margin_amount: u64, multiplier_bps: u64) -> Result<u64> {
    let notional = (margin_amount as u128)
        .checked_mul(multiplier_bps as u128)
        .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let debt = notional
        .checked_sub(margin_amount as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(debt > 0, ErrorCode::AmountZero);
    u64::try_from(debt).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn spot_value_at_curve_price(
    market: &Market,
    collateral_asset: MarketAsset,
    amount: u64,
    base_price_nad: u64,
) -> Result<u64> {
    require!(base_price_nad > 0, ErrorCode::InsufficientLiquidity);
    let amount_nad = normalize_to_nad(amount as u128, market.side(collateral_asset).asset_decimals)?;
    let value_nad = match collateral_asset {
        MarketAsset::Base => amount_nad
            .checked_mul(base_price_nad as u128)
            .and_then(|value| value.checked_div(crate::constants::NAD as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?,
        MarketAsset::Quote => amount_nad
            .checked_mul(crate::constants::NAD as u128)
            .and_then(|value| value.checked_div(base_price_nad as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?,
    };
    denormalize_from_nad_floor(value_nad, market.side(collateral_asset.opposite()).asset_decimals)
}

fn unwind_impact_bps(spot_value: u64, closeout_value: u64) -> Result<u128> {
    require!(spot_value > 0, ErrorCode::InsufficientLiquidity);
    if closeout_value >= spot_value {
        return Ok(0);
    }
    Ok((spot_value as u128)
        .checked_sub(closeout_value as u128)
        .and_then(|value| value.checked_mul(BPS_DENOMINATOR as u128))
        .and_then(|value| value.checked_div(spot_value as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?)
}

fn equity_bps(closeout_value: u64, debt_amount: u64) -> Result<u128> {
    if closeout_value == 0 {
        return Ok(0);
    }
    Ok((closeout_value.saturating_sub(debt_amount) as u128)
        .checked_mul(BPS_DENOMINATOR as u128)
        .and_then(|value| value.checked_div(closeout_value as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?)
}

fn require_initial_leverage_health(
    market: &Market,
    collateral_asset: MarketAsset,
    collateral_amount: u64,
    base_price_nad: u64,
    closeout_value: u64,
    debt_amount: u64,
) -> Result<()> {
    require_gt!(closeout_value, debt_amount, ErrorCode::LeverageInitialMarginTooLow);
    let margin_bps = equity_bps(closeout_value, debt_amount)?;
    require_gte!(
        margin_bps,
        LEVERAGE_INITIAL_MARGIN_BPS as u128,
        ErrorCode::LeverageInitialMarginTooLow
    );
    let spot_value = spot_value_at_curve_price(market, collateral_asset, collateral_amount, base_price_nad)?;
    let unwind_bps = unwind_impact_bps(spot_value, closeout_value)?;
    require_gte!(
        LEVERAGE_MAX_UNWIND_IMPACT_BPS as u128,
        unwind_bps,
        ErrorCode::LeverageUnwindImpactTooHigh
    );
    Ok(())
}

fn require_leverage_not_liquidatable(closeout_value: u64, debt_amount: u64) -> Result<()> {
    let margin_bps = equity_bps(closeout_value, debt_amount)?;
    require!(
        closeout_value > debt_amount && margin_bps > LEVERAGE_MAINTENANCE_BUFFER_BPS as u128,
        ErrorCode::LeveragePositionNotLiquidatable
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    include!("../../tests/state/leverage.rs");
}
