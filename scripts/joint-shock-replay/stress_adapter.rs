// Compiled only into the copied off-chain benchmark engine under target.
// Mirrors backstop_liquidation_auction.rs for transfer-fee-free SPL assets.
// Existing benchmark floor execution rejects hLP token settlements; this adapter
// performs those settlements using its existing, identity-checked helper.
#[derive(Clone, Debug)]
pub struct StressFloorReceipt {
    pub preview: BenchmarkLiquidationPreview,
    pub base_rebalance: HlpRebalanceReceipt,
    pub quote_rebalance: HlpRebalanceReceipt,
    pub cash: BenchmarkCashFlow,
    pub pre_loss_supply: u64,
    pub pre_loss_hlp_shares: [u64; 2],
}

impl BenchmarkMarket {
    pub fn stress_floor_with_hlp(
        &mut self,
        position: &mut BenchmarkBorrowPosition,
        base_external: &mut BenchmarkHlpOwnedState,
        quote_external: &mut BenchmarkHlpOwnedState,
        request: BenchmarkLiquidationPlanRequest,
        protocol_interest_fee_bps: u16,
    ) -> Result<StressFloorReceipt> {
        self.require_monotonic_clock(request.clock)?;
        self.assert_position_market(position)?;
        base_external.validate(self)?;
        quote_external.validate(self)?;
        let mut next = clone_market(&self.market)?;
        let mut pos = clone_borrow_position(&position.position)?;
        let mut base = base_external.try_fork()?;
        let mut quote = quote_external.try_fork()?;
        let custody_before = [
            required_reserve_custody(&next.base_side)?,
            required_reserve_custody(&next.quote_side)?,
        ];
        advance_market_to_slot(&mut next, request.clock.slot)?;
        let (plan, mut prepared) =
            prepare_floor_liquidation_plan(&mut next, &mut pos, request, self.transfer_fees)?;
        let prepared = prepared
            .as_deref_mut()
            .ok_or(ErrorCode::InsufficientAmount)?;
        let (finalized, internal) = next.settle_backstop_swap(
            Some(prepared),
            crate::transitions::amm::LendingSwapSettlement {
                position: &mut pos,
                debt_asset: plan.debt_asset,
                insurance_spent: plan.insurance_draw_debit,
                insurance_credit: plan.insurance_draw_debit,
                collateral_consumed: plan.collateral_consumed,
                caller_bounty: plan.caller_bounty,
            },
            request.clock.slot,
            request.protocol_swap_fee_bps,
            request.protocol_auction_split,
        )?;
        let finalized = finalized.ok_or(ErrorCode::BrokenInvariant)?;
        // The atomic settlement changes ownership before the public loss.
        // Its final observation changes the cache, not ownership; these are
        // therefore the yLP weights at the loss event.
        let pre_loss_supply = next.base_side.shares.ylp_supply;
        let pre_loss_hlp_shares = [
            next.base_hlp_vault.ylp_shares,
            next.quote_hlp_vault.ylp_shares,
        ];
        let mut cash = BenchmarkCashFlow::default();
        let transfer = |amount| BenchmarkTokenTransferOutcome {
            source_debit: amount,
            destination_credit: amount,
        };
        apply_hlp_rebalance_settlement(
            &mut next,
            &mut base,
            finalized.base_rebalance,
            prepared.interest_eligibility,
            transfer(finalized.base_rebalance.interest_paid),
            protocol_interest_fee_bps,
            request.protocol_auction_split,
            &mut cash,
        )?;
        apply_hlp_rebalance_settlement(
            &mut next,
            &mut quote,
            finalized.quote_rebalance,
            prepared.interest_eligibility,
            transfer(finalized.quote_rebalance.interest_paid),
            protocol_interest_fee_bps,
            request.protocol_auction_split,
            &mut cash,
        )?;
        let public_interest = internal.liquidation.interest_paid;
        next.side_mut(plan.debt_asset).record_interest_credit(
            public_interest,
            protocol_interest_fee_bps,
            request.protocol_auction_split,
            0,
        )?;
        let debt_cash = cash.side_mut(plan.debt_asset);
        debt_cash.reserve_vault_credit += plan.insurance_draw_debit;
        debt_cash.reserve_vault_debit += public_interest + internal.owner_residual;
        debt_cash.interest_vault_credit += public_interest;
        debt_cash.insurance_vault_debit += plan.insurance_draw_debit;
        debt_cash.recipient_credit += internal.owner_residual;
        let collateral_cash = cash.side_mut(plan.debt_asset.opposite());
        collateral_cash.reserve_vault_credit += plan.collateral_reserve_credit;
        collateral_cash.collateral_vault_debit += plan.collateral_consumed;
        collateral_cash.recipient_credit += plan.caller_bounty;
        for (i, side_cash) in [cash.base, cash.quote].into_iter().enumerate() {
            let actual = custody_before[i]
                .checked_add(side_cash.reserve_vault_credit)
                .and_then(|x| x.checked_sub(side_cash.reserve_vault_debit))
                .ok_or(ErrorCode::BrokenInvariant)?;
            let side = if i == 0 {
                &next.base_side
            } else {
                &next.quote_side
            };
            require_eq!(
                actual,
                required_reserve_custody(side)?,
                ErrorCode::BrokenInvariant
            );
        }
        let key = self.require_market_key()?;
        validate_hlp_owned_state(&base, &next, key)?;
        validate_hlp_owned_state(&quote, &next, key)?;
        let out = StressFloorReceipt {
            preview: BenchmarkLiquidationPreview {
                plan,
                native: internal.liquidation,
                owner_residual: internal.owner_residual,
            },
            base_rebalance: finalized.base_rebalance,
            quote_rebalance: finalized.quote_rebalance,
            cash,
            pre_loss_supply,
            pre_loss_hlp_shares,
        };
        *self.market = next;
        *position.position = pos;
        self.clock = request.clock;
        *base_external = base;
        *quote_external = quote;
        Ok(out)
    }

    /// Full holder fee entitlements after native yield checkpoints, without
    /// harvesting or changing the live scenario. Whole raw atoms only.
    pub fn stress_hlp_yield(&self, external: &BenchmarkHlpOwnedState) -> Result<[u64; 2]> {
        let mut next = clone_market(&self.market)?;
        next.checkpoint_hlp_yield_from_ylp(external.target_asset)?;
        let mut ext = external.try_fork()?;
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let (swap, interest) = next.hlp_yield_growth_indexes(external.target_asset, asset);
            let account = if asset == MarketAsset::Base {
                &mut ext.base_yield_account
            } else {
                &mut ext.quote_yield_account
            };
            account.accrue(ext.holder_hlp_token_balance, swap, interest)?;
        }
        Ok([
            ext.base_yield_account.accrued_swap_fee_amount
                + ext.base_yield_account.accrued_interest_amount,
            ext.quote_yield_account.accrued_swap_fee_amount
                + ext.quote_yield_account.accrued_interest_amount,
        ])
    }
}

impl BenchmarkMarket {
    pub fn stress_start_ledger_gap(&self) -> Result<[i128; 2]> {
        let state = self.market.integrated_curve_state_nad()?;
        let mut out = [0i128; 2];
        for (i, asset) in [MarketAsset::Base, MarketAsset::Quote]
            .into_iter()
            .enumerate()
        {
            let ordinary = if i == 0 {
                state.ordinary_base
            } else {
                state.ordinary_quote
            };
            let equity = if i == 0 {
                state.base_hlp_equity
            } else {
                state.quote_hlp_equity
            };
            let debt = if i == 0 {
                Debt::shares_to_debt(
                    self.market.quote_hlp_vault.debt_shares,
                    self.market.debt.base_borrow_index_nad,
                )?
            } else {
                Debt::shares_to_debt(
                    self.market.base_hlp_vault.debt_shares,
                    self.market.debt.quote_borrow_index_nad,
                )?
            };
            let ledger = self.market.normalize_amount(
                self.market.curve_reserve(asset)? as u128,
                self.market.side(asset).asset_decimals,
            )? - self
                .market
                .normalize_amount(debt, self.market.side(asset).asset_decimals)?;
            out[i] = (ordinary + equity) as i128 - ledger as i128;
        }
        Ok(out)
    }
    pub fn stress_opposite_gaps(&self) -> Result<[i128; 2]> {
        let m = &self.market;
        let supply = m.base_side.shares.ylp_supply as u128;
        let base = m.curve_reserve(MarketAsset::Quote)? as u128
            * m.base_hlp_vault.ylp_shares as u128
            / supply;
        let quote = m.curve_reserve(MarketAsset::Base)? as u128
            * m.quote_hlp_vault.ylp_shares as u128
            / supply;
        Ok([
            base as i128
                - Debt::shares_to_debt(m.base_hlp_vault.debt_shares, m.debt.quote_borrow_index_nad)?
                    as i128,
            quote as i128
                - Debt::shares_to_debt(m.quote_hlp_vault.debt_shares, m.debt.base_borrow_index_nad)?
                    as i128,
        ])
    }
}
