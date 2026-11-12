use super::liquidity::{prepare_concentrated_hlp_transition, prepare_concentrated_hlp_transition_at_current_state};
use crate::constants::LIQUIDATION_INCENTIVE_BPS;

impl Market {
    pub fn quote_leverage_swap(
        &self,
        asset_in: MarketAsset,
        amount_in: u64,
        current_slot: u64,
    ) -> Result<LeverageSwapQuote> {
        let after_launch = self.config.start_time.saturating_add(
            self.config
                .amm
                .launch_fee_duration_seconds
                .max(self.config.amm.launch_rate_limit_duration_seconds)
                .min(i64::MAX as u64) as i64,
        );
        self.quote_leverage_swap_at_time(asset_in, amount_in, current_slot, after_launch)
    }

    pub fn liquidate_leverage_position(
        &mut self,
        position: &mut LeveragePosition,
        prepared_swap: Option<PreparedLeverageSwap>,
        measured_unwind_credit: u64,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
    ) -> Result<LeverageLiquidationReceipt> {
        self.liquidate_leverage_position_with_pending_credit(
            position,
            prepared_swap,
            measured_unwind_credit,
            None,
            swap_fee_credit,
            protocol_fee_bps,
            protocol_auction_split,
            current_slot,
        )
    }

    pub fn leverage_closeout_value(&self, position: &LeveragePosition, current_slot: u64) -> Result<u64> {
        self.leverage_closeout_value_with_fee(position, current_slot, LeverageCollateralFee::default())
    }

    pub fn leverage_closeout_value_at_time(
        &self,
        position: &LeveragePosition,
        current_slot: u64,
        current_unix_timestamp: i64,
    ) -> Result<u64> {
        self.leverage_closeout_value_at_time_with_fee(
            position,
            current_slot,
            current_unix_timestamp,
            LeverageCollateralFee::default(),
        )
    }

    /// Exact executable output used by protection health. Only valuation is
    /// returned; the execution path still constructs and validates a full swap.
    pub fn leverage_protection_closeout_value(
        &self,
        position: &LeveragePosition,
        current_slot: u64,
        current_unix_timestamp: i64,
    ) -> Result<u64> {
        self.leverage_protection_closeout_value_with_fee(
            position,
            current_slot,
            current_unix_timestamp,
            LeverageCollateralFee::default(),
        )
    }
}

impl Market {
    #[allow(clippy::too_many_arguments)]
    pub fn liquidate_leverage_position_with_pending_credit(
        &mut self,
        position: &mut LeveragePosition,
        prepared_swap: Option<PreparedLeverageSwap>,
        measured_unwind_credit: u64,
        pending_unwind_credit: Option<u64>,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
    ) -> Result<LeverageLiquidationReceipt> {
        if prepared_swap.is_none() {
            require_eq!(measured_unwind_credit, 0, ErrorCode::BrokenInvariant);
            require!(
                swap_fee_credit == LeverageSwapFeeCredit::default(),
                ErrorCode::BrokenInvariant
            );
            position.require_open()?;
            let debt_asset = position.debt_asset()?;
            let collateral_asset = debt_asset.opposite();
            let debt_amount = position.debt_amount(&self.debt)?;
            require_gt!(debt_amount, 0, ErrorCode::ZeroDebtAmount);
            let collateral_sold = position.collateral_amount;
            self.ensure_amm_initialized(current_slot)?;

            let lifecycle = self.apply_leverage_lifecycle_transition(
                SwapCashPolicy::Liquidate {
                    debt_asset,
                    debt_shares: position.debt_shares,
                    debt_principal: position.debt_principal,
                },
                collateral_asset,
                0,
                0,
                0,
            )?;
            self.apply_leverage_socialized_loss(debt_asset, lifecycle, current_slot)?;
            let transition = prepare_concentrated_hlp_transition_at_current_state(self)?;
            let (base_hlp_rebalance, quote_hlp_rebalance) = transition.consume(self)?;
            self.finalize_amm_socialized_loss_and_observe_risk(current_slot)?;
            self.assert_market_invariants()?;

            position.debt_shares = lifecycle.position_debt_shares;
            position.debt_principal = lifecycle.position_debt_principal;
            self.reduce_leverage_collateral(position, collateral_sold)?;
            return Ok(LeverageLiquidationReceipt {
                debt_repaid: lifecycle.clearance.cash_repaid,
                socialized_loss: lifecycle.socialized_principal_loss,
                interest_paid: lifecycle.clearance.interest_paid,
                principal_written_off: lifecycle.writeoff.principal_written_off,
                collateral_sold,
                closeout_value: 0,
                liquidator_amount: 0,
                owner_residual: 0,
                swap: LeverageSwapQuote {
                    asset_in: collateral_asset.code(),
                    quoted_slot: current_slot,
                    ..LeverageSwapQuote::default()
                },
                fees: FeesReceipt::default(),
                base_hlp_rebalance,
                quote_hlp_rebalance,
            });
        }
        let mut prepared_swap = prepared_swap.ok_or(ErrorCode::BrokenInvariant)?;
        let swap = prepared_swap.leverage_quote();
        position.require_open()?;
        let debt_asset = position.debt_asset()?;
        let debt_amount = position.debt_amount(&self.debt)?;
        require_gt!(debt_amount, 0, ErrorCode::ZeroDebtAmount);
        let collateral_sold = position.collateral_amount;
        self.ensure_amm_initialized(current_slot)?;
        require!(
            swap.amount_in > 0 && swap.amount_in <= collateral_sold,
            ErrorCode::BrokenInvariant
        );
        require_eq!(swap.amount_in, measured_unwind_credit, ErrorCode::BrokenInvariant);
        let maintenance_bps = position.margin_terms.maintenance_bps(position.collateral_amount)?;
        let collateral_asset = position.collateral_asset()?;
        let pending_unhealthy = pending_unwind_credit
            .map(|credit| {
                require_gte!(position.collateral_amount, credit, ErrorCode::BrokenInvariant);
                Ok(
                    self.ema_leverage_margin_bps_for_credit(collateral_asset, credit, debt_amount)?
                        <= u128::from(maintenance_bps),
                )
            })
            .transpose()?
            .unwrap_or(false);
        if !pending_unhealthy {
            require!(
                self.ema_leverage_margin_bps_for_credit(collateral_asset, measured_unwind_credit, debt_amount,)?
                    <= u128::from(maintenance_bps),
                ErrorCode::LeveragePositionNotLiquidatable
            );
        }

        let cash_policy = SwapCashPolicy::Liquidate {
            debt_asset,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
        };
        require!(
            prepared_swap.cash_policy
                == SwapCashPolicy::Liquidate {
                    debt_asset,
                    debt_shares: position.debt_shares,
                    debt_principal: position.debt_principal,
                },
            ErrorCode::BrokenInvariant
        );
        prepared_swap.cash_policy = cash_policy;
        let finalized = prepared_swap.apply(
            self,
            cash_policy,
            swap_fee_credit,
            current_slot,
            protocol_fee_bps,
            protocol_auction_split,
            None,
        )?;
        let lifecycle = finalized.lifecycle;
        let fees = finalized.fees;
        let (base_hlp_rebalance, quote_hlp_rebalance) = (finalized.base_rebalance, finalized.quote_rebalance);
        let clearance = lifecycle.clearance;
        let writeoff = lifecycle.writeoff;
        position.debt_shares = lifecycle.position_debt_shares;
        position.debt_principal = lifecycle.position_debt_principal;
        let full_cash_repayment = clearance
            .cash_repaid
            .checked_add(writeoff.aggregate_debt_written_off)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let residual = swap.amount_out.saturating_sub(full_cash_repayment);
        let max_incentive = (debt_amount as u128)
            .checked_mul(LIQUIDATION_INCENTIVE_BPS as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)? as u64;
        let liquidator_amount = residual.min(max_incentive);
        let owner_residual = residual
            .checked_sub(liquidator_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.reduce_leverage_collateral(position, collateral_sold)?;
        Ok(LeverageLiquidationReceipt {
            debt_repaid: clearance.cash_repaid,
            socialized_loss: lifecycle.socialized_principal_loss,
            interest_paid: clearance.interest_paid,
            principal_written_off: writeoff.principal_written_off,
            collateral_sold,
            closeout_value: swap.amount_out,
            liquidator_amount,
            owner_residual,
            swap,
            fees,
            base_hlp_rebalance,
            quote_hlp_rebalance,
        })
    }
}
