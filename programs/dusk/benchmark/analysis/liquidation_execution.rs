pub struct Liquidation {
    pub debt_asset: MarketAsset,
    pub repay_credit: u64,
    pub max_socialized_loss: u64,
    pub terms: LiquidationTerms,
    pub pricing: LiquidationPricing,
    pub collateral_fee: Option<LeverageCollateralFee>,
    pub auction_eligibility_fee: Option<LeverageCollateralFee>,
    pub exact_net_auction_pricing: bool,
}

fn position_debt(market: &Market, borrow_position: &BorrowPosition, debt_asset: MarketAsset) -> Result<u128> {
    match debt_asset {
        MarketAsset::Base => borrow_position.fixed_base_debt(&market.debt),
        MarketAsset::Quote => borrow_position.fixed_quote_debt(&market.debt),
    }
}

fn collateral_amount_for_debt_value_with_pricing(
    market: &Market,
    debt_asset: MarketAsset,
    debt_amount: u64,
    penalty_bps: u16,
    pricing: LiquidationPricing,
) -> Result<u64> {
    match pricing {
        LiquidationPricing::PessimisticReserves => {
            let risk = market.current_risk()?;
            require_gte!(penalty_bps, LIQUIDATION_INCENTIVE_BPS, ErrorCode::InvalidMarketConfig);
            let debt_with_penalty = ceil_div(
                (debt_amount as u128)
                    .checked_mul((BPS_DENOMINATOR + penalty_bps) as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                BPS_DENOMINATOR as u128,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?;
            let collateral_asset = debt_asset.opposite();
            let debt_amount_nad = market.normalize_amount(debt_with_penalty, market.side(debt_asset).asset_decimals)?;
            let (geometry, point, direction) = market
                .pessimistic_concentrated_curve(collateral_asset, &risk, true)?
                .ok_or(ErrorCode::BrokenInvariant)?;
            let collateral_amount_nad = geometry.quote_exact_out(point, debt_amount_nad, direction)?.amount_in;
            market.denormalize_amount_ceil(collateral_amount_nad, market.side(collateral_asset).asset_decimals)
        }
        LiquidationPricing::ReferencePrice {
            debt_per_collateral_price_nad,
        } => {
            require!(debt_per_collateral_price_nad > 0, ErrorCode::InvalidSettlementPrice);
            let debt_decimals = market.side(debt_asset).asset_decimals;
            let collateral_decimals = market.side(debt_asset.opposite()).asset_decimals;
            let debt_with_penalty = ceil_div(
                (debt_amount as u128)
                    .checked_mul((BPS_DENOMINATOR + penalty_bps) as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                BPS_DENOMINATOR as u128,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?;
            let debt_value_nad = market.normalize_amount(debt_with_penalty, debt_decimals)?;
            let collateral_amount_nad = ceil_div(
                debt_value_nad
                    .checked_mul(NAD as u128)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                debt_per_collateral_price_nad as u128,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?;
            market.denormalize_amount_ceil(collateral_amount_nad, collateral_decimals)
        }
    }
}



#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LiquidationDebtClearance {
    shares_to_burn: u128,
    aggregate_debt_reduction: u64,
}

impl Liquidation {
    pub fn new_with_pricing(
        debt_asset: MarketAsset,
        repay_credit: u64,
        max_socialized_loss: u64,
        terms: LiquidationTerms,
        pricing: LiquidationPricing,
    ) -> Self {
        Self {
            debt_asset,
            repay_credit,
            max_socialized_loss,
            terms,
            pricing,
            collateral_fee: None,
            auction_eligibility_fee: None,
            exact_net_auction_pricing: false,
        }
    }

    pub fn apply(self, market: &mut Market, borrow_position: &mut BorrowPosition) -> Result<LiquidationReceipt> {
        borrow_position.require_idle()?;
        let unpaid_before = market.public_unpaid_interest(self.debt_asset)?;
        let debt_before = position_debt(market, borrow_position, self.debt_asset)?;
        require_gte!(debt_before, self.repay_credit as u128, ErrorCode::InsufficientDebt);
        require_gte!(
            self.terms.max_repay_amount,
            self.repay_credit,
            ErrorCode::LiquidationRepayTooLarge
        );
        let collateral_before = position_collateral(borrow_position, self.debt_asset);
        let total_collateral_quote = collateral_amount_for_debt_value_with_pricing(
            market,
            self.debt_asset,
            self.repay_credit,
            self.terms.total_penalty_bps,
            self.pricing,
        )?;
        let liquidator_collateral_quote = collateral_amount_for_debt_value_with_pricing(
            market,
            self.debt_asset,
            self.repay_credit,
            self.terms.liquidation_incentive_bps,
            self.pricing,
        )?;
        let (collateral_seized, collateral_to_liquidator) = if self.exact_net_auction_pricing {
            let fee = self.collateral_fee.ok_or(ErrorCode::BrokenInvariant)?;
            let liquidator_gross = fee.effective_gross_for_credit(liquidator_collateral_quote)?;
            require_gte!(collateral_before, liquidator_gross, ErrorCode::LiquidationRepayTooLarge);
            let junior_net = total_collateral_quote.saturating_sub(liquidator_collateral_quote);
            let junior_gross = fee.effective_gross_for_credit(junior_net)?;
            let seized = liquidator_gross
                .checked_add(junior_gross)
                .ok_or(ErrorCode::MarketMathOverflow)?
                .min(collateral_before);
            (seized, liquidator_gross)
        } else {
            let seized = total_collateral_quote.min(collateral_before);
            (seized, liquidator_collateral_quote.min(seized))
        };
        let junior_fee_funded = collateral_seized
            .checked_sub(collateral_to_liquidator)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let collateral_exhausted = collateral_seized == collateral_before;
        let cash_repayment = self.repay_credit as u128;
        require_gte!(
            self.terms.max_repay_amount,
            self.repay_credit,
            ErrorCode::LiquidationRepayTooLarge
        );
        let debt_clearance = {
            let (shares_before, position_debt_before, borrow_index_nad) = match self.debt_asset {
                MarketAsset::Base => (
                    borrow_position.fixed_base_shares,
                    borrow_position.fixed_base_debt(&market.debt)?,
                    market.debt.base_borrow_index_nad,
                ),
                MarketAsset::Quote => (
                    borrow_position.fixed_quote_shares,
                    borrow_position.fixed_quote_debt(&market.debt)?,
                    market.debt.quote_borrow_index_nad,
                ),
            };
            require!(
                shares_before > 0 && position_debt_before > 0,
                ErrorCode::InsufficientDebt
            );
            let repayment = if collateral_exhausted {
                Debt::repayment_for_max(
                    shares_before,
                    match self.debt_asset {
                        MarketAsset::Base => market.debt.fixed_base_shares,
                        MarketAsset::Quote => market.debt.fixed_quote_shares,
                    },
                    borrow_index_nad,
                    u64::MAX,
                )?
            } else {
                let cash_repaid = u64::try_from(cash_repayment).map_err(|_| ErrorCode::MarketMathOverflow)?;
                let quote = Debt::repayment_for_max(
                    shares_before,
                    match self.debt_asset {
                        MarketAsset::Base => market.debt.fixed_base_shares,
                        MarketAsset::Quote => market.debt.fixed_quote_shares,
                    },
                    borrow_index_nad,
                    cash_repaid,
                )?;
                require_eq!(quote.cash_repaid, cash_repaid, ErrorCode::DebtShareDivisionOverflow);
                quote
            };
            LiquidationDebtClearance {
                shares_to_burn: repayment.shares_to_burn,
                aggregate_debt_reduction: repayment.cash_repaid,
            }
        };
        let cash_repaid = u64::try_from(cash_repayment).map_err(|_| ErrorCode::MarketMathOverflow)?;
        require_gte!(
            debt_clearance.aggregate_debt_reduction,
            cash_repaid,
            ErrorCode::MarketMathOverflow
        );
        let socialized_loss = if collateral_exhausted {
            debt_clearance
                .aggregate_debt_reduction
                .checked_sub(cash_repaid)
                .ok_or(ErrorCode::MarketMathOverflow)?
        } else {
            0
        };
        require_gte!(
            self.max_socialized_loss,
            socialized_loss,
            ErrorCode::LiquidationSocializationExceeded
        );
        // Track the principal/interest split for cash-backed repayment without
        // treating socialized loss or share-rounding writeoff as received
        // interest.
        let (interest_paid, principal_credit) = market.clear_fixed_position_principal(
            borrow_position,
            self.debt_asset,
            cash_repaid,
            debt_clearance.shares_to_burn,
            debt_clearance.aggregate_debt_reduction,
        )?;
        match self.debt_asset {
            MarketAsset::Base => {
                borrow_position.quote_collateral = borrow_position
                    .quote_collateral
                    .checked_sub(collateral_seized)
                    .ok_or(ErrorCode::InsufficientBalance)?;
                borrow_position.fixed_base_shares = borrow_position
                    .fixed_base_shares
                    .checked_sub(debt_clearance.shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                market.debt.fixed_base_shares = market
                    .debt
                    .fixed_base_shares
                    .checked_sub(debt_clearance.shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
            MarketAsset::Quote => {
                borrow_position.base_collateral = borrow_position
                    .base_collateral
                    .checked_sub(collateral_seized)
                    .ok_or(ErrorCode::InsufficientBalance)?;
                borrow_position.fixed_quote_shares = borrow_position
                    .fixed_quote_shares
                    .checked_sub(debt_clearance.shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                market.debt.fixed_quote_shares = market
                    .debt
                    .fixed_quote_shares
                    .checked_sub(debt_clearance.shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
        }

        let live_debit = debt_clearance
            .aggregate_debt_reduction
            .checked_sub(principal_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        market.settle_public_debt_reserves(
            self.debt_asset,
            unpaid_before,
            interest_paid,
            live_debit,
            principal_credit,
        )?;
        if junior_fee_funded > 0 {
            market
                .side_mut(self.debt_asset.opposite())
                .record_junior_liquidation_credit(
                    self.collateral_fee
                        .unwrap_or_default()
                        .unwind_credit(junior_fee_funded)?,
                )?;
        }

        market.refresh_risk()?;
        let remaining_debt = position_debt(market, borrow_position, self.debt_asset)?;
        let remaining_collateral = position_collateral(borrow_position, self.debt_asset);
        let collateral_fee = self.collateral_fee.unwrap_or_default();
        let remaining_collateral_effective = collateral_fee.unwind_credit(remaining_collateral)?;
        let target_contribution = market.debt_capped_global_health_contribution(
            self.debt_asset,
            remaining_debt,
            remaining_collateral,
            &market.risk,
        )?;
        if remaining_debt == 0 {
            borrow_position.set_liquidation_cf_bps(self.debt_asset, 0);
            borrow_position.clear_referral_binding(self.debt_asset);
        } else if remaining_collateral_effective > 0 {
            let total_debt_nad = market.total_fixed_debt_nad(self.debt_asset)?;
            let external_debt_nad = market.external_fixed_debt_nad(borrow_position, self.debt_asset)?;
            let projected_aggregate = fee_discounted_contribution(
                market.projected_aggregate_global_health_contribution(
                    borrow_position,
                    self.debt_asset,
                    target_contribution,
                )?,
                collateral_fee.haircut_bps(),
            );
            let terms = market.dynamic_borrow_terms(
                self.debt_asset,
                remaining_collateral_effective,
                external_debt_nad,
                total_debt_nad,
                projected_aggregate,
                &market.risk,
            )?;
            borrow_position.set_liquidation_cf_bps(self.debt_asset, terms.liquidation_cf_bps);
        } else {
            // With a 100% exit fee the position remains unhealthy. Retain its
            // issued threshold until the fee or position state changes.
            require_gt!(
                borrow_position.liquidation_cf_bps(self.debt_asset),
                0,
                ErrorCode::BrokenInvariant
            );
        }
        market.reconcile_global_health_contribution(borrow_position, self.debt_asset, target_contribution)?;
        if self.collateral_fee.is_some() {
            let auction_credit = self
                .auction_eligibility_fee
                .unwrap_or(collateral_fee)
                .unwind_credit(remaining_collateral)?;
            market.reconcile_liquidation_auction_with_credit(borrow_position, auction_credit)?;
        } else {
            market.reconcile_liquidation_auction(borrow_position)?;
        }

        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;

        Ok(LiquidationReceipt {
            repaid_amount: self.repay_credit,
            interest_paid,
            collateral_seized,
            collateral_to_liquidator,
            junior_fee_funded,
            socialized_loss,
            remaining_debt: position_debt(market, borrow_position, self.debt_asset)?,
            remaining_global_health_contribution: borrow_position.global_health_contribution(self.debt_asset),
            remaining_liquidation_cf_bps: borrow_position.liquidation_cf_bps(self.debt_asset),
            liquidation_incentive_bps: self.terms.liquidation_incentive_bps,
            junior_fee_bps: self.terms.junior_fee_bps,
            max_repay_amount: self.terms.max_repay_amount,
        })
    }
}
