impl Market {
    /// Historical auction replay only. Runtime liquidation uses the flash policy.
    pub(crate) fn settle_backstop_swap(
        &mut self,
        prepared: Option<&mut PreparedSwap>,
        settlement: LendingSwapSettlement<'_>,
        current_slot: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<(Option<FinalizedSwapState>, InternalLiquidationReceipt)> {
        if let Some(prepared) = prepared {
            let policy = SwapCashPolicy::Liquidate {
                debt_asset: settlement.debt_asset,
                debt_shares: 0,
                debt_principal: 0,
            };
            let credit = LeverageSwapFeeCredit::from_total_actual_credit(
                &prepared.leverage_quote(),
                prepared.quote.fee.claimable_fee_debit,
            )?;
            let finalized = prepared.apply(
                self,
                policy,
                credit,
                current_slot,
                protocol_fee_bps,
                protocol_auction_split,
                Some(settlement),
            )?;
            let receipt = finalized.liquidation.ok_or(ErrorCode::BrokenInvariant)?;
            Ok((Some(finalized), receipt))
        } else {
            let receipt = settlement.apply(self, 0)?;
            if receipt.liquidation.socialized_loss > 0 {
                self.finalize_amm_socialized_loss_and_observe_risk(current_slot)?;
            } else {
                self.finalize_amm_transition_and_observe_risk(current_slot)?;
            }
            Ok((None, receipt))
        }
    }
}
