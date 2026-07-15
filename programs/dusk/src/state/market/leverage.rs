use anchor_lang::prelude::*;

use super::{FeesReceipt, Market, MarketAsset, ProtocolAuctionSplit};
use crate::{
    constants::{
        BPS_DENOMINATOR, LEVERAGE_INITIAL_MARGIN_BPS, LEVERAGE_MAINTENANCE_BUFFER_BPS, LEVERAGE_MAX_MULTIPLIER_BPS,
        LEVERAGE_MAX_UNWIND_IMPACT_BPS, LIQUIDATION_INCENTIVE_BPS,
    },
    errors::ErrorCode,
    math::{calculate_raw_amount_in, calculate_raw_amount_out},
    shared::math::ceil_div,
    state::{LeverageMarginMode, LeveragePosition},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageSwapQuote {
    pub amount_in: u64,
    pub amount_in_after_fee: u64,
    pub amount_out: u64,
    pub fee_credit: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageOpenReceipt {
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
    pub fn quote_leverage_swap(&self, asset_in: MarketAsset, amount_in: u64) -> Result<LeverageSwapQuote> {
        require!(amount_in > 0, ErrorCode::AmountZero);
        let total_fee = leverage_swap_fee(amount_in, self.config.swap_fee_bps)?;
        let amount_in_after_fee = amount_in.checked_sub(total_fee).ok_or(ErrorCode::FeeMathOverflow)?;
        require!(amount_in_after_fee > 0, ErrorCode::InsufficientOutputAmount);
        let (side_in, side_out) = self.swap_sides(asset_in);
        let amount_out = calculate_raw_amount_out(
            side_in.reserves.live_reserve,
            side_out.reserves.live_reserve,
            amount_in_after_fee,
        )?;
        Ok(LeverageSwapQuote {
            amount_in,
            amount_in_after_fee,
            amount_out,
            fee_credit: total_fee,
        })
    }

    pub fn quote_leverage_swap_exact_output(
        &self,
        asset_in: MarketAsset,
        amount_out: u64,
    ) -> Result<LeverageSwapQuote> {
        require!(amount_out > 0, ErrorCode::AmountZero);
        let (side_in, side_out) = self.swap_sides(asset_in);
        require_gt!(
            side_out.reserves.live_reserve,
            amount_out,
            ErrorCode::InsufficientLiquidity
        );
        let minimum_after_fee = calculate_raw_amount_in(
            side_in.reserves.live_reserve,
            side_out.reserves.live_reserve,
            amount_out,
        )?;
        require!(minimum_after_fee > 0, ErrorCode::InsufficientOutputAmount);
        let amount_in = gross_up_leverage_swap_fee(minimum_after_fee, self.config.swap_fee_bps)?;
        let fee_credit = leverage_swap_fee(amount_in, self.config.swap_fee_bps)?;
        let amount_in_after_fee = amount_in.checked_sub(fee_credit).ok_or(ErrorCode::FeeMathOverflow)?;
        require_gte!(
            amount_in_after_fee,
            minimum_after_fee,
            ErrorCode::InsufficientOutputAmount
        );
        require_gte!(
            calculate_raw_amount_out(
                side_in.reserves.live_reserve,
                side_out.reserves.live_reserve,
                amount_in_after_fee,
            )?,
            amount_out,
            ErrorCode::InsufficientOutputAmount
        );
        Ok(LeverageSwapQuote {
            amount_in,
            amount_in_after_fee,
            amount_out,
            fee_credit,
        })
    }

    pub fn open_leverage(
        &mut self,
        position: &mut LeveragePosition,
        owner: Pubkey,
        market: Pubkey,
        position_id: Pubkey,
        debt_asset: MarketAsset,
        margin_credit: u64,
        multiplier_bps: u64,
        collateral_credit: u64,
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
        let debt_amount = leverage_debt_from_margin(margin_credit, multiplier_bps)?;
        let notional = margin_credit
            .checked_add(debt_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let swap = self.quote_leverage_swap(debt_asset, notional)?;
        require_gte!(swap.amount_out, collateral_credit, ErrorCode::SlippageExceeded);
        require!(collateral_credit > 0, ErrorCode::InsufficientOutputAmount);

        let closeout_value =
            self.post_swap_closeout_value(debt_asset, notional, debt_asset.opposite(), collateral_credit)?;
        require_initial_leverage_health(
            collateral_credit,
            self.post_swap_reserve(debt_asset.opposite(), debt_asset, swap.amount_out)?,
            self.post_swap_reserve(debt_asset, debt_asset, swap.amount_in_after_fee)?,
            closeout_value,
            debt_amount,
        )?;
        {
            let debt_side = self.side_mut(debt_asset)?;
            debt_side.reserves.cash_reserve = debt_side
                .reserves
                .cash_reserve
                .checked_sub(debt_amount)
                .ok_or(ErrorCode::CashReserveUnderflow)?;
        }
        let fees = self.apply_leverage_swap(
            debt_asset,
            swap,
            swap.amount_out,
            0,
            manager_fee_bps,
            protocol_fee_bps,
            protocol_auction_split,
        )?;
        let debt_shares = self.debt.add_isolated_debt(debt_asset, debt_amount)?;
        position.initialize(
            owner,
            market,
            position_id,
            debt_asset,
            collateral_credit,
            margin_credit,
            notional,
            debt_amount,
            debt_shares,
            multiplier_bps,
            opened_at,
            opened_slot,
            bump,
        );
        let equity = closeout_value
            .checked_sub(debt_amount)
            .ok_or(ErrorCode::LeverageInitialMarginTooLow)?;
        Ok(LeverageOpenReceipt {
            debt_amount,
            debt_shares,
            notional,
            collateral_amount: collateral_credit,
            closeout_value,
            equity,
            swap,
            fees,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open_collateral_margin_leverage(
        &mut self,
        position: &mut LeveragePosition,
        owner: Pubkey,
        market: Pubkey,
        position_id: Pubkey,
        debt_asset: MarketAsset,
        margin_credit: u64,
        multiplier_bps: u64,
        supplemental_amount_out: u64,
        supplemental_collateral_credit: u64,
        max_debt_in: u64,
        opened_at: i64,
        opened_slot: u64,
        bump: u8,
        manager_fee_bps: u16,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<LeverageOpenReceipt> {
        let target_collateral = leverage_target_collateral_from_margin(margin_credit, multiplier_bps)?;
        let supplemental_target = target_collateral
            .checked_sub(margin_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(
            supplemental_collateral_credit,
            supplemental_target,
            ErrorCode::InsufficientOutputAmount
        );
        require_gte!(
            supplemental_amount_out,
            supplemental_collateral_credit,
            ErrorCode::InsufficientOutputAmount
        );
        let collateral_amount = margin_credit
            .checked_add(supplemental_collateral_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let swap = self.quote_leverage_swap_exact_output(debt_asset, supplemental_amount_out)?;
        require_gte!(max_debt_in, swap.amount_in, ErrorCode::SlippageExceeded);

        let closeout_value =
            self.post_swap_closeout_value_with_quote(debt_asset, swap, debt_asset.opposite(), collateral_amount)?;
        require_initial_leverage_health(
            collateral_amount,
            self.post_swap_reserve(debt_asset.opposite(), debt_asset, swap.amount_out)?,
            self.post_swap_reserve(debt_asset, debt_asset, swap.amount_in_after_fee)?,
            closeout_value,
            swap.amount_in,
        )?;
        {
            let debt_side = self.side_mut(debt_asset)?;
            debt_side.reserves.cash_reserve = debt_side
                .reserves
                .cash_reserve
                .checked_sub(swap.amount_in)
                .ok_or(ErrorCode::CashReserveUnderflow)?;
        }
        let (applied_swap, fees) = self.apply_leverage_swap_exact_output(
            debt_asset,
            supplemental_amount_out,
            swap.amount_out,
            0,
            manager_fee_bps,
            protocol_fee_bps,
            protocol_auction_split,
        )?;
        require!(applied_swap == swap, ErrorCode::BrokenInvariant);
        let debt_shares = self.debt.add_isolated_debt(debt_asset, swap.amount_in)?;
        position.initialize_with_margin_mode(
            owner,
            market,
            position_id,
            debt_asset,
            LeverageMarginMode::Collateral,
            collateral_amount,
            margin_credit,
            target_collateral,
            swap.amount_in,
            debt_shares,
            multiplier_bps,
            opened_at,
            opened_slot,
            bump,
        );
        let equity = closeout_value
            .checked_sub(swap.amount_in)
            .ok_or(ErrorCode::LeverageInitialMarginTooLow)?;
        Ok(LeverageOpenReceipt {
            debt_amount: swap.amount_in,
            debt_shares,
            notional: target_collateral,
            collateral_amount,
            closeout_value,
            equity,
            swap,
            fees,
        })
    }

    pub fn increase_leverage(
        &mut self,
        position: &mut LeveragePosition,
        debt_amount: u64,
        collateral_credit: u64,
        manager_fee_bps: u16,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<LeverageUpdateReceipt> {
        position.require_open()?;
        require!(debt_amount > 0, ErrorCode::AmountZero);
        require!(collateral_credit > 0, ErrorCode::InsufficientOutputAmount);
        let debt_asset = position.debt_asset()?;
        let debt_before = position.debt_amount(&self.debt)?;
        let swap = self.quote_leverage_swap(debt_asset, debt_amount)?;
        require_gte!(swap.amount_out, collateral_credit, ErrorCode::SlippageExceeded);
        let collateral_after = position
            .collateral_amount
            .checked_add(collateral_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let debt_after = debt_before
            .checked_add(debt_amount)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let closeout_value =
            self.post_swap_closeout_value(debt_asset, debt_amount, debt_asset.opposite(), collateral_after)?;
        require_initial_leverage_health(
            collateral_after,
            self.post_swap_reserve(debt_asset.opposite(), debt_asset, swap.amount_out)?,
            self.post_swap_reserve(debt_asset, debt_asset, swap.amount_in_after_fee)?,
            closeout_value,
            debt_after,
        )?;
        {
            let debt_side = self.side_mut(debt_asset)?;
            debt_side.reserves.cash_reserve = debt_side
                .reserves
                .cash_reserve
                .checked_sub(debt_amount)
                .ok_or(ErrorCode::CashReserveUnderflow)?;
        }
        let fees = self.apply_leverage_swap(
            debt_asset,
            swap,
            swap.amount_out,
            0,
            manager_fee_bps,
            protocol_fee_bps,
            protocol_auction_split,
        )?;
        let added_shares = self.debt.add_isolated_debt(debt_asset, debt_amount)?;
        position.debt_shares = position
            .debt_shares
            .checked_add(added_shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        position.debt_principal = position
            .debt_principal
            .checked_add(debt_amount as u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        position.credit_collateral(collateral_credit)?;
        Ok(LeverageUpdateReceipt {
            debt_delta: i64::try_from(debt_amount).map_err(|_| ErrorCode::Overflow)?,
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
        manager_fee_bps: u16,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
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
        let swap = self.quote_leverage_swap(collateral_asset, collateral_debit)?;
        require_gte!(swap.amount_out, min_repay_out, ErrorCode::SlippageExceeded);
        require_gt!(debt_before, swap.amount_out, ErrorCode::InsufficientDebt);
        let collateral_after = position
            .collateral_amount
            .checked_sub(collateral_debit)
            .ok_or(ErrorCode::InsufficientAmount)?;
        let debt_after = debt_before
            .checked_sub(swap.amount_out)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let closeout_value =
            self.post_swap_closeout_value_with_quote(collateral_asset, swap, collateral_asset, collateral_after)?;
        require_leverage_not_liquidatable(closeout_value, debt_after)?;
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
            manager_fee_bps,
            protocol_fee_bps,
            protocol_auction_split,
        )?;
        position.debit_collateral(collateral_debit)?;
        Ok(LeverageUpdateReceipt {
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
        manager_fee_bps: u16,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<LeverageCloseReceipt> {
        position.require_open()?;
        let debt_asset = position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        let debt_amount = position.debt_amount(&self.debt)?;
        require_gt!(debt_amount, 0, ErrorCode::ZeroDebtAmount);
        let collateral_sold = position.collateral_amount;
        let swap = self.quote_leverage_swap(collateral_asset, collateral_sold)?;
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
            manager_fee_bps,
            protocol_fee_bps,
            protocol_auction_split,
        )?;
        position.collateral_amount = 0;
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

    pub fn close_collateral_margin_leverage(
        &mut self,
        position: &mut LeveragePosition,
        collateral_debit: u64,
        max_collateral_in: u64,
        manager_fee_bps: u16,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<LeverageCloseReceipt> {
        position.require_open()?;
        position.require_margin_mode(LeverageMarginMode::Collateral)?;
        require!(collateral_debit > 0, ErrorCode::AmountZero);
        let debt_asset = position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        let debt_amount = position.debt_amount(&self.debt)?;
        require_gt!(debt_amount, 0, ErrorCode::ZeroDebtAmount);
        let swap = self.quote_leverage_swap_exact_output(collateral_asset, debt_amount)?;
        require_gte!(
            collateral_debit,
            swap.amount_in,
            ErrorCode::UnexpectedTokenTransferAmount
        );
        require_gte!(max_collateral_in, collateral_debit, ErrorCode::SlippageExceeded);
        require_gte!(
            position.collateral_amount,
            collateral_debit,
            ErrorCode::InsufficientAmount
        );
        let residual = position
            .collateral_amount
            .checked_sub(collateral_debit)
            .ok_or(ErrorCode::InsufficientAmount)?;
        let clearance = self.debt.clear_isolated_debt(
            debt_asset,
            &mut position.debt_shares,
            &mut position.debt_principal,
            debt_amount,
        )?;
        let live_debit = clearance.live_debit_for_cash_repay()?;
        let (applied_swap, fees) = self.apply_leverage_swap_exact_output(
            collateral_asset,
            debt_amount,
            clearance.interest_paid,
            live_debit,
            manager_fee_bps,
            protocol_fee_bps,
            protocol_auction_split,
        )?;
        require!(applied_swap == swap, ErrorCode::BrokenInvariant);
        position.collateral_amount = 0;
        Ok(LeverageCloseReceipt {
            debt_repaid: debt_amount,
            interest_paid: clearance.interest_paid,
            collateral_sold: collateral_debit,
            closeout_value: debt_amount,
            residual,
            swap,
            fees,
        })
    }

    pub fn liquidate_leverage(
        &mut self,
        position: &mut LeveragePosition,
        manager_fee_bps: u16,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<LeverageLiquidationReceipt> {
        position.require_open()?;
        let debt_asset = position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        let debt_amount = position.debt_amount(&self.debt)?;
        require_gt!(debt_amount, 0, ErrorCode::ZeroDebtAmount);
        let collateral_sold = position.collateral_amount;
        let swap = self.quote_leverage_swap(collateral_asset, collateral_sold)?;
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
            manager_fee_bps,
            protocol_fee_bps,
            protocol_auction_split,
        )?;
        if writeoff.debt_written_off > 0 {
            let debt_side = self.side_mut(debt_asset)?;
            debt_side.reserves.live_reserve = debt_side
                .reserves
                .live_reserve
                .checked_sub(writeoff.debt_written_off)
                .ok_or(ErrorCode::ReserveUnderflow)?;
            debt_side.assert_share_backing()?;
        }
        position.collateral_amount = 0;
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
    ) -> Result<LeverageUpdateReceipt> {
        position.require_open()?;
        require!(repay_credit > 0, ErrorCode::AmountZero);
        let debt_asset = position.debt_asset()?;
        let debt_before = position.debt_amount(&self.debt)?;
        require_gt!(debt_before, repay_credit, ErrorCode::InsufficientDebt);
        let closeout_value = self.leverage_closeout_value(position)?;
        let debt_after = debt_before
            .checked_sub(repay_credit)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        require_leverage_not_liquidatable(closeout_value, debt_after)?;
        let clearance = self.debt.clear_isolated_debt(
            debt_asset,
            &mut position.debt_shares,
            &mut position.debt_principal,
            repay_credit,
        )?;
        let principal_paid = clearance.principal_paid;
        let live_debit = clearance.live_debit_for_cash_repay()?;
        let side = self.side_mut(debt_asset)?;
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
        Ok(LeverageUpdateReceipt {
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
    ) -> Result<LeverageUpdateReceipt> {
        position.require_open()?;
        require!(borrow_amount > 0, ErrorCode::AmountZero);
        let debt_asset = position.debt_asset()?;
        let debt_before = position.debt_amount(&self.debt)?;
        let debt_after = debt_before
            .checked_add(borrow_amount)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let closeout_value = self.leverage_closeout_value(position)?;
        require_initial_leverage_health(
            position.collateral_amount,
            self.side(position.collateral_asset()?)?.reserves.live_reserve,
            self.side(debt_asset)?.reserves.live_reserve,
            closeout_value,
            debt_after,
        )?;
        let side = self.side_mut(debt_asset)?;
        side.reserves.cash_reserve = side
            .reserves
            .cash_reserve
            .checked_sub(borrow_amount)
            .ok_or(ErrorCode::CashReserveUnderflow)?;
        let shares = self.debt.add_isolated_debt(debt_asset, borrow_amount)?;
        position.debt_shares = position
            .debt_shares
            .checked_add(shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        position.debt_principal = position
            .debt_principal
            .checked_add(borrow_amount as u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        Ok(LeverageUpdateReceipt {
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

    pub fn deposit_leverage_collateral(
        &self,
        position: &mut LeveragePosition,
        collateral_credit: u64,
    ) -> Result<LeverageUpdateReceipt> {
        position.require_open()?;
        require!(collateral_credit > 0, ErrorCode::AmountZero);
        let collateral_after = position
            .collateral_amount
            .checked_add(collateral_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let closeout_value = self
            .quote_leverage_swap(position.collateral_asset()?, collateral_after)?
            .amount_out;
        position.credit_collateral(collateral_credit)?;
        Ok(LeverageUpdateReceipt {
            debt_delta: 0,
            collateral_delta: i64::try_from(collateral_credit).map_err(|_| ErrorCode::Overflow)?,
            debt_amount: position.debt_amount(&self.debt)?,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: 0,
            fees: FeesReceipt::default(),
        })
    }

    pub fn withdraw_leverage_collateral(
        &self,
        position: &mut LeveragePosition,
        collateral_debit: u64,
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
        let collateral_after = position
            .collateral_amount
            .checked_sub(collateral_debit)
            .ok_or(ErrorCode::InsufficientAmount)?;
        let debt_amount = position.debt_amount(&self.debt)?;
        let closeout_value = self.quote_leverage_swap(collateral_asset, collateral_after)?.amount_out;
        require_initial_leverage_health(
            collateral_after,
            self.side(collateral_asset)?.reserves.live_reserve,
            self.side(debt_asset)?.reserves.live_reserve,
            closeout_value,
            debt_amount,
        )?;
        position.debit_collateral(collateral_debit)?;
        Ok(LeverageUpdateReceipt {
            debt_delta: 0,
            collateral_delta: -i64::try_from(collateral_debit).map_err(|_| ErrorCode::Overflow)?,
            debt_amount,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: 0,
            fees: FeesReceipt::default(),
        })
    }

    pub fn leverage_closeout_value(&self, position: &LeveragePosition) -> Result<u64> {
        let collateral_asset = position.collateral_asset()?;
        self.quote_leverage_swap(collateral_asset, position.collateral_amount)
            .map(|quote| quote.amount_out)
    }

    fn post_swap_closeout_value(
        &self,
        asset_in: MarketAsset,
        amount_in: u64,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
    ) -> Result<u64> {
        let swap = self.quote_leverage_swap(asset_in, amount_in)?;
        self.post_swap_closeout_value_with_quote(asset_in, swap, collateral_asset, collateral_amount)
    }

    fn post_swap_closeout_value_with_quote(
        &self,
        asset_in: MarketAsset,
        swap: LeverageSwapQuote,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
    ) -> Result<u64> {
        let debt_asset = collateral_asset.opposite();
        let collateral_reserve = self.post_swap_reserve_for(collateral_asset, asset_in, swap)?;
        let debt_reserve = self.post_swap_reserve_for(debt_asset, asset_in, swap)?;
        let total_fee = ceil_div(
            (collateral_amount as u128)
                .checked_mul(self.config.swap_fee_bps as u128)
                .ok_or(ErrorCode::FeeMathOverflow)?,
            BPS_DENOMINATOR as u128,
        )
        .ok_or(ErrorCode::FeeMathOverflow)?
        .min(collateral_amount as u128) as u64;
        let after_fee = collateral_amount
            .checked_sub(total_fee)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        calculate_raw_amount_out(collateral_reserve, debt_reserve, after_fee)
    }

    fn post_swap_reserve(&self, asset: MarketAsset, asset_in: MarketAsset, delta: u64) -> Result<u64> {
        let reserve = self.side(asset)?.reserves.live_reserve;
        if asset == asset_in {
            reserve.checked_add(delta).ok_or(ErrorCode::ReserveOverflow.into())
        } else {
            reserve.checked_sub(delta).ok_or(ErrorCode::ReserveUnderflow.into())
        }
    }

    fn post_swap_reserve_for(&self, asset: MarketAsset, asset_in: MarketAsset, swap: LeverageSwapQuote) -> Result<u64> {
        if asset == asset_in {
            self.post_swap_reserve(asset, asset_in, swap.amount_in_after_fee)
        } else {
            self.post_swap_reserve(asset, asset_in, swap.amount_out)
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply_leverage_swap_exact_output(
        &mut self,
        asset_in: MarketAsset,
        amount_out: u64,
        cash_debit_out: u64,
        extra_live_debit_out: u64,
        manager_fee_bps: u16,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<(LeverageSwapQuote, FeesReceipt)> {
        let swap = self.quote_leverage_swap_exact_output(asset_in, amount_out)?;
        let fees = self.apply_leverage_swap(
            asset_in,
            swap,
            cash_debit_out,
            extra_live_debit_out,
            manager_fee_bps,
            protocol_fee_bps,
            protocol_auction_split,
        )?;
        Ok((swap, fees))
    }

    fn apply_leverage_swap(
        &mut self,
        asset_in: MarketAsset,
        swap: LeverageSwapQuote,
        cash_debit_out: u64,
        extra_live_debit_out: u64,
        manager_fee_bps: u16,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<FeesReceipt> {
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
        let fees = side_in.record_swap_fee_credit(
            swap.fee_credit,
            manager_fee_bps,
            protocol_fee_bps,
            protocol_auction_split,
        )?;
        side_in.assert_share_backing()?;
        side_out.assert_share_backing()?;
        Ok(fees)
    }
}

pub(crate) fn leverage_debt_from_margin(margin_amount: u64, multiplier_bps: u64) -> Result<u64> {
    let notional = leverage_target_collateral_from_margin(margin_amount, multiplier_bps)?;
    let debt = notional
        .checked_sub(margin_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(debt > 0, ErrorCode::AmountZero);
    Ok(debt)
}

pub(crate) fn leverage_target_collateral_from_margin(margin_amount: u64, multiplier_bps: u64) -> Result<u64> {
    require!(margin_amount > 0, ErrorCode::AmountZero);
    require!(multiplier_bps > BPS_DENOMINATOR as u64, ErrorCode::InvalidArgument);
    require!(
        multiplier_bps <= LEVERAGE_MAX_MULTIPLIER_BPS,
        ErrorCode::LeverageMultiplierTooHigh
    );
    let notional = (margin_amount as u128)
        .checked_mul(multiplier_bps as u128)
        .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let notional = u64::try_from(notional).map_err(|_| ErrorCode::MarketMathOverflow)?;
    require_gt!(notional, margin_amount, ErrorCode::AmountZero);
    Ok(notional)
}

fn leverage_swap_fee(amount_in: u64, swap_fee_bps: u16) -> Result<u64> {
    let fee = ceil_div(
        (amount_in as u128)
            .checked_mul(swap_fee_bps as u128)
            .ok_or(ErrorCode::FeeMathOverflow)?,
        BPS_DENOMINATOR as u128,
    )
    .ok_or(ErrorCode::FeeMathOverflow)?
    .min(amount_in as u128);
    u64::try_from(fee).map_err(|_| ErrorCode::FeeMathOverflow.into())
}

fn gross_up_leverage_swap_fee(amount_in_after_fee: u64, swap_fee_bps: u16) -> Result<u64> {
    require!(amount_in_after_fee > 0, ErrorCode::AmountZero);
    let denominator = BPS_DENOMINATOR
        .checked_sub(swap_fee_bps)
        .ok_or(ErrorCode::InvalidSwapFeeBps)?;
    require!(denominator > 0, ErrorCode::InsufficientOutputAmount);
    let amount_in = ceil_div(
        (amount_in_after_fee as u128)
            .checked_mul(BPS_DENOMINATOR as u128)
            .ok_or(ErrorCode::FeeMathOverflow)?,
        denominator as u128,
    )
    .ok_or(ErrorCode::FeeMathOverflow)?;
    u64::try_from(amount_in).map_err(|_| ErrorCode::FeeMathOverflow.into())
}

fn spot_value_from_reserves(amount: u64, collateral_reserve: u64, debt_reserve: u64) -> Result<u64> {
    require!(
        collateral_reserve > 0 && debt_reserve > 0,
        ErrorCode::InsufficientLiquidity
    );
    let value = (amount as u128)
        .checked_mul(debt_reserve as u128)
        .and_then(|value| value.checked_div(collateral_reserve as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow.into())
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
    collateral_amount: u64,
    collateral_reserve: u64,
    debt_reserve: u64,
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
    let spot_value = spot_value_from_reserves(collateral_amount, collateral_reserve, debt_reserve)?;
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
