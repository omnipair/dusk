use anchor_lang::prelude::*;

use crate::{
    constants::{BPS_DENOMINATOR, NAD},
    errors::ErrorCode,
    math::{mul_div_ceil_u128, mul_div_u128},
    state::*,
    transitions::{DebtRepaymentQuote, LeverageCollateralFee},
};

pub(crate) enum LiquidationPositionRef<'a> {
    Borrow(&'a BorrowPosition),
    Leverage(&'a LeveragePosition),
}

impl LiquidationPositionRef<'_> {
    pub(crate) fn collateral(&self, asset: MarketAsset) -> u64 {
        match self {
            Self::Borrow(p) => p.collateral(asset.opposite()),
            Self::Leverage(p) => p.collateral_amount,
        }
    }

    pub(crate) fn shares(&self, asset: MarketAsset) -> u128 {
        match self {
            Self::Borrow(p) => p.debt_shares(asset),
            Self::Leverage(p) => p.debt_shares,
        }
    }

    pub(crate) fn maintenance(&self, asset: MarketAsset, collateral: u64) -> Result<u16> {
        match self {
            Self::Borrow(p) => Ok(BPS_DENOMINATOR
                .saturating_sub(p.liquidation_cf_bps(asset))
                .clamp(1, 9_999)),
            Self::Leverage(p) => p.margin_terms.maintenance_bps(collateral),
        }
    }

    pub(crate) fn age(&self, asset: MarketAsset, now: i64) -> Result<u64> {
        match self {
            Self::Borrow(p) => p.distress(asset).age(now),
            Self::Leverage(p) => p.distress.age(now),
        }
    }

    pub(crate) fn require_idle(&self) -> Result<()> {
        match self {
            Self::Borrow(p) => p.require_idle(),
            Self::Leverage(p) => p.require_idle(),
        }
    }

    /// A flash quote fixes the burned shares. The cash bound rounds those
    /// shares up, so a different aggregate floor phase at settle cannot make
    /// another position's repayment invalidate the quote. Any unused atom
    /// belongs to the liquidated owner, not to the buyer or protocol.
    fn bounded_repayment(&self, market: &Market, asset: MarketAsset, max_amount: u64) -> Result<DebtRepaymentQuote> {
        let index = market.debt.borrow_index(asset);
        require_gte!(index, NAD as u128, ErrorCode::DebtShareDivisionOverflow);
        let shares = mul_div_u128(max_amount as u128, NAD as u128, index)?.min(self.shares(asset));
        self.bounded_share_repayment(market, asset, shares)
    }

    fn bounded_share_repayment(&self, market: &Market, asset: MarketAsset, shares: u128) -> Result<DebtRepaymentQuote> {
        require_gt!(shares, 0, ErrorCode::DebtShareDivisionOverflow);
        let remaining = self
            .shares(asset)
            .checked_sub(shares)
            .ok_or(ErrorCode::InsufficientDebt)?;
        let index = market.debt.borrow_index(asset);
        let remaining_debt = Debt::shares_to_debt(remaining, index)?;
        Ok(DebtRepaymentQuote {
            shares_to_burn: shares,
            cash_repaid: u64::try_from(mul_div_ceil_u128(shares, index, NAD as u128)?)
                .map_err(|_| ErrorCode::DebtMathOverflow)?,
            position_debt_reduced: u64::try_from(Debt::shares_to_debt(self.shares(asset), index)? - remaining_debt)
                .map_err(|_| ErrorCode::DebtMathOverflow)?,
            remaining_position_debt: u64::try_from(remaining_debt).map_err(|_| ErrorCode::DebtMathOverflow)?,
        })
    }
}

impl Market {
    pub(crate) fn reset_borrow_distress_if_recovered(
        &self,
        position: &mut BorrowPosition,
        asset: MarketAsset,
        fee: LeverageCollateralFee,
    ) -> Result<()> {
        if !position.distress(asset).active {
            return Ok(());
        }
        if position.debt_shares(asset) == 0
            || !self
                .flash_liquidation_health(&LiquidationPositionRef::Borrow(position), asset, fee)?
                .eligible()?
        {
            *position.distress_mut(asset) = LiquidationDistress {
                observed_slot: position.distress(asset).observed_slot,
                ..LiquidationDistress::default()
            };
        }
        Ok(())
    }

    pub(crate) fn reset_leverage_distress_if_recovered(
        &self,
        position: &mut LeveragePosition,
        fee: LeverageCollateralFee,
    ) -> Result<()> {
        if !position.distress.active {
            return Ok(());
        }
        if position.debt_shares == 0
            || !self
                .flash_liquidation_health(&LiquidationPositionRef::Leverage(position), position.debt_asset()?, fee)?
                .eligible()?
        {
            position.distress = LiquidationDistress {
                observed_slot: position.distress.observed_slot,
                ..LiquidationDistress::default()
            };
        }
        Ok(())
    }

    pub(crate) fn liquidation_value(&self, asset: MarketAsset, collateral_credit: u64) -> Result<u64> {
        let value =
            self.linear_liquidation_collateral_value_nad(asset.opposite(), collateral_credit, &self.current_risk()?)?;
        self.denormalize_amount_floor(value, self.side(asset).asset_decimals)
    }

    pub(crate) fn flash_liquidation_health(
        &self,
        position: &LiquidationPositionRef<'_>,
        asset: MarketAsset,
        fee: LeverageCollateralFee,
    ) -> Result<LiquidationHealth> {
        if let LiquidationPositionRef::Leverage(p) = position {
            require!(p.debt_asset()? == asset, ErrorCode::InvalidLeveragePosition);
        }
        let collateral = position.collateral(asset);
        let collateral_value = self.liquidation_value(asset, fee.unwind_credit(collateral)?)?;
        let debt = u64::try_from(Debt::shares_to_debt(
            position.shares(asset),
            self.debt.borrow_index(asset),
        )?)
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        Ok(LiquidationHealth {
            collateral_value,
            debt,
            maintenance_bps: position.maintenance(asset, collateral.max(1))?,
        })
    }

    pub fn liquidation_insurance_target(&self, asset: MarketAsset) -> Result<u64> {
        let principal = match asset {
            MarketAsset::Base => {
                u128::from(self.debt.fixed_base_principal) + u128::from(self.debt.isolated_base_principal)
            }
            MarketAsset::Quote => {
                u128::from(self.debt.fixed_quote_principal) + u128::from(self.debt.isolated_quote_principal)
            }
        };
        let funding = match asset {
            MarketAsset::Base => &self.quote_hlp_vault,
            MarketAsset::Quote => &self.base_hlp_vault,
        };
        liquidation_insurance_target(
            principal,
            Debt::shares_to_debt(funding.debt_shares, self.debt.borrow_index(asset))?,
        )
    }

    pub(crate) fn quote_flash_liquidation(
        &self,
        position: &LiquidationPositionRef<'_>,
        asset: MarketAsset,
        max_repayment: u64,
        full: bool,
        collateral_fee: LeverageCollateralFee,
        eligibility_fee: LeverageCollateralFee,
        debt_fee: LeverageCollateralFee,
        now: i64,
        slot: u64,
    ) -> Result<FlashLiquidationQuote> {
        position.require_idle()?;
        let health_before = self.flash_liquidation_health(position, asset, eligibility_fee)?;
        require!(health_before.eligible()?, ErrorCode::PositionNotLiquidatable);
        let rates = health_before.rates(position.age(asset, now)?)?;
        let available = position.collateral(asset);
        let target = self.liquidation_insurance_target(asset)?;
        let balance = self.insurance.available(asset);
        let full_debt = position.bounded_repayment(self, asset, u64::MAX)?;
        let (collateral_debit, collateral_credit, repayment, fee, owner_surplus, debt_shares_to_burn, remaining_debt) =
            if full {
                require!(health_before.emergency_allowed()?, ErrorCode::PositionNotLiquidatable);
                require!(
                    health_before.debt >= health_before.collateral_value
                        || self.proves_no_useful_partial(
                            position,
                            asset,
                            collateral_fee,
                            eligibility_fee,
                            debt_fee,
                            now,
                            slot
                        )?,
                    ErrorCode::LiquidationRepayTooLarge
                );
                let credit = collateral_fee.unwind_credit(available)?;
                let value = self.liquidation_value(asset, credit)?;
                let purchase = u64::try_from(mul_div_ceil_u128(
                    value as u128,
                    (10_000 - rates.buyer_discount_bps) as u128,
                    10_000,
                )?)
                .map_err(|_| ErrorCode::MarketMathOverflow)?;
                let repayment = purchase.min(full_debt.cash_repaid);
                let surplus = purchase.saturating_sub(repayment);
                let fee = LiquidationFeeAllocation::quote(repayment, surplus, balance, target);
                (
                    available,
                    credit,
                    repayment,
                    fee,
                    surplus - fee.total,
                    position.shares(asset),
                    0,
                )
            } else {
                let requested = position.bounded_repayment(self, asset, max_repayment)?;
                let minimum = match asset {
                    MarketAsset::Base => self.config.liquidation.minimum_base_debt,
                    MarketAsset::Quote => self.config.liquidation.minimum_quote_debt,
                };
                let retained_shares = if health_before.debt >= minimum {
                    mul_div_ceil_u128(u128::from(minimum), u128::from(NAD), self.debt.borrow_index(asset))?.max(1)
                } else {
                    1
                };
                let maximum_burn = position
                    .shares(asset)
                    .checked_sub(retained_shares)
                    .ok_or(ErrorCode::LiquidationResidualTooSmall)?;
                let repay =
                    position.bounded_share_repayment(self, asset, requested.shares_to_burn.min(maximum_burn))?;
                let fee = LiquidationFeeAllocation::quote(repay.cash_repaid, u64::MAX, balance, target);
                let debit = self.flash_collateral_debit_at_price(
                    asset,
                    repay.cash_repaid,
                    fee.total,
                    rates.buyer_discount_bps,
                    collateral_fee,
                    self.pessimistic_collateral_price_nad(asset.opposite(), &self.current_risk()?, false),
                )?;
                require!(debit < available, ErrorCode::LiquidationRepayTooLarge);
                let minimum = match asset {
                    MarketAsset::Base => self.config.liquidation.minimum_base_debt,
                    MarketAsset::Quote => self.config.liquidation.minimum_quote_debt,
                };
                require!(
                    repay.remaining_position_debt >= minimum || health_before.debt < minimum,
                    ErrorCode::LiquidationResidualTooSmall
                );
                (
                    debit,
                    collateral_fee.unwind_credit(debit)?,
                    repay.cash_repaid,
                    fee,
                    0,
                    repay.shares_to_burn,
                    repay.remaining_position_debt,
                )
            };
        let remaining_collateral = available - collateral_debit;
        let health_after = LiquidationHealth {
            collateral_value: self.liquidation_value(asset, eligibility_fee.unwind_credit(remaining_collateral)?)?,
            debt: remaining_debt,
            maintenance_bps: position.maintenance(asset, remaining_collateral.max(1))?,
        };
        if !full {
            require!(
                health_before.improves(health_after)?,
                ErrorCode::LiquidationDoesNotImproveHealth
            );
            if health_after.recovered()? && debt_shares_to_burn > 1 {
                self.require_first_flash_recovery(
                    position,
                    asset,
                    debt_shares_to_burn,
                    rates.buyer_discount_bps,
                    collateral_fee,
                    eligibility_fee,
                )?;
            }
        }
        let mut payment = 0_u64;
        // Reserve two owner atoms for aggregate debt/fee rounding at settle.
        // Unused custody is refunded to the buyer. Grossing this reserve up
        // matters for capped transfer fees: splitting one transfer into two
        // may cost more than the reduction in the original transfer.
        let owner_bound = owner_surplus.checked_add(2).ok_or(ErrorCode::MarketMathOverflow)?;
        for net in [repayment, fee.insurance, fee.lp + fee.protocol, owner_bound] {
            if net > 0 {
                payment = payment
                    .checked_add(debt_fee.effective_gross_for_credit(net)?)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
        }
        Ok(FlashLiquidationQuote {
            full,
            collateral_debit,
            collateral_credit,
            payment,
            repayment,
            owner_surplus,
            fee,
            debt_shares_to_burn,
            health_before,
            health_after,
            rates,
        })
    }

    /// Certify the first recovery crossing over all smaller integer share
    /// burns. A local predecessor check is insufficient: atom rounding can
    /// cross, dip below the target, and cross again at a larger sale.
    #[inline(never)]
    fn require_first_flash_recovery(
        &self,
        position: &LiquidationPositionRef<'_>,
        asset: MarketAsset,
        shares: u128,
        discount: u16,
        collateral_fee: LeverageCollateralFee,
        eligibility_fee: LeverageCollateralFee,
    ) -> Result<()> {
        let risk = self.current_risk()?;
        let price = self.pessimistic_collateral_price_nad(asset.opposite(), &risk, false);
        let health = |burn| -> Result<LiquidationHealth> {
            let repayment = position.bounded_share_repayment(self, asset, burn)?;
            let debit = self.flash_collateral_debit_at_price(
                asset,
                repayment.cash_repaid,
                repayment.cash_repaid / 100,
                discount,
                collateral_fee,
                price,
            )?;
            let remaining = position
                .collateral(asset)
                .checked_sub(debit)
                .ok_or(ErrorCode::LiquidationRepayTooLarge)?;
            Ok(LiquidationHealth {
                collateral_value: self.denormalize_amount_floor(
                    self.linear_liquidation_collateral_value_nad(
                        asset.opposite(),
                        eligibility_fee.unwind_credit(remaining)?,
                        &risk,
                    )?,
                    self.side(asset).asset_decimals,
                )?,
                debt: repayment.remaining_position_debt,
                maintenance_bps: position.maintenance(asset, remaining.max(1))?,
            })
        };
        let mut pending = vec![(1, shares - 1)];
        for _ in 0..2048 {
            let Some((lo, hi)) = pending.pop() else {
                return Ok(());
            };
            let left = health(lo)?;
            let right = if lo == hi { left } else { health(hi)? };
            let optimistic = LiquidationHealth {
                collateral_value: left.collateral_value,
                debt: right.debt,
                maintenance_bps: right.maintenance_bps,
            };
            if !optimistic.recovered()? {
                continue;
            }
            require!(lo != hi, ErrorCode::LiquidationRepayTooLarge);
            let mid = lo + (hi - lo) / 2;
            pending.push((mid + 1, hi));
            pending.push((lo, mid));
        }
        err!(ErrorCode::LiquidationRepayTooLarge)
    }

    fn flash_collateral_debit_at_price(
        &self,
        asset: MarketAsset,
        repayment: u64,
        fee: u64,
        discount: u16,
        collateral_fee: LeverageCollateralFee,
        price: u64,
    ) -> Result<u64> {
        let obligation = repayment.checked_add(fee).ok_or(ErrorCode::MarketMathOverflow)?;
        let value = mul_div_ceil_u128(obligation as u128, 10_000, (10_000 - discount) as u128)?;
        let normalized = self.normalize_amount(value, self.side(asset).asset_decimals)?;
        let normalized_collateral = mul_div_ceil_u128(normalized, NAD as u128, price as u128)?;
        let credit = self.denormalize_amount_ceil(normalized_collateral, self.side(asset.opposite()).asset_decimals)?;
        collateral_fee.effective_gross_for_credit(credit)
    }
}
