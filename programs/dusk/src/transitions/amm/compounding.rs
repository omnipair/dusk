//! Endpoint-valued fee compounding with distinct senior/junior ownership.
use super::*;

pub(super) struct CompoundedTrancheFee {
    pub asset: MarketAsset,
    pub amount: u64,
    pub claimable_lp: u64,
    pub price: u64,
}

impl Market {
    // Separate SBF frame: the swap evaluator already holds both curve quotes.
    #[inline(never)]
    pub(super) fn apply_compounded_tranche_fee(
        &self,
        state: &IntegratedCurveState,
        end: &mut IntegratedCurveState,
        input: CompoundedTrancheFee,
    ) -> Result<()> {
        let CompoundedTrancheFee {
            asset: fee_asset,
            amount: compounded_fee_debit,
            claimable_lp: claimable_lp_fee,
            price: end_price_nad,
        } = input;
        if !self.side(fee_asset).fees.junior.initialized {
            let amount = self.normalize_amount(compounded_fee_debit as u128, self.side(fee_asset).asset_decimals)?;
            apply_compounded_ylp_fee_to_end(
                *state,
                end,
                fee_asset == MarketAsset::Base,
                amount,
                end_price_nad as u128,
                self.base_side.shares.ylp_supply,
                self.base_hlp_vault.ylp_shares,
                self.quote_hlp_vault.ylp_shares,
                amount,
            )?;
        } else {
            let mut carry = self.side(fee_asset).fees.junior.carry;

            // Settlement publishes claimable fees first, then compounds the
            // remaining LP allocation using the same indivisible-atom carry.
            carry.allocate(
                claimable_lp_fee,
                crate::state::TrancheRevenueSource::SwapFee,
                &self.tranches,
            )?;
            let allocation = carry.allocate(
                compounded_fee_debit,
                crate::state::TrancheRevenueSource::SwapFee,
                &self.tranches,
            )?;
            let capitalized_fee = compounded_fee_debit - allocation.rebuild_seed;
            let total = self.base_side.shares.ylp_supply;
            let senior = self.tranches.senior_reserve_shares(total)?;
            let active_hlp = (if state.frozen_hlp_mask & 1 == 0 {
                self.base_hlp_vault.ylp_shares
            } else {
                0
            })
            .checked_add(if state.frozen_hlp_mask & 2 == 0 {
                self.quote_hlp_vault.ylp_shares
            } else {
                0
            })
            .ok_or(ErrorCode::SupplyOverflow)?;
            let static_supply = total.checked_sub(active_hlp).ok_or(ErrorCode::SupplyUnderflow)?;
            let static_senior = static_supply
                .checked_sub(self.tranches.junior_reserve_shares)
                .ok_or(ErrorCode::SupplyUnderflow)?;
            let value = |state: &IntegratedCurveState| -> Result<u128> {
                let (base, quote) = state.book_ordinary()?;
                mul_div_u128(base, end_price_nad as u128, NAD as u128)?
                    .checked_add(quote)
                    .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
            };
            let before = value(end)?;
            apply_compounded_ylp_fee_to_end(
                *state,
                end,
                fee_asset == MarketAsset::Base,
                self.normalize_amount(capitalized_fee as u128, self.side(fee_asset).asset_decimals)?,
                end_price_nad as u128,
                senior,
                self.base_hlp_vault.ylp_shares,
                self.quote_hlp_vault.ylp_shares,
                self.normalize_amount(allocation.senior as u128, self.side(fee_asset).asset_decimals)?,
            )?;
            let after = value(end)?;
            let senior_fee = self.normalize_amount(allocation.senior as u128, self.side(fee_asset).asset_decimals)?;
            let senior_fee_value = if fee_asset == MarketAsset::Base {
                mul_div_u128(senior_fee, end_price_nad as u128, NAD as u128)?
            } else {
                senior_fee
            };
            let new_static = if static_senior == 0 || self.tranches.junior_reserve_shares == 0 {
                static_supply
            } else {
                let senior_value = mul_div_u128(before, static_senior as u128, static_supply as u128)?
                    .checked_add(mul_div_u128(senior_fee_value, static_senior as u128, senior as u128)?)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                u64::try_from(mul_div_u128(static_senior as u128, after, senior_value)?)
                    .map_err(|_| ErrorCode::SupplyOverflow)?
            };
            let new_junior = new_static
                .checked_sub(static_senior)
                .ok_or(ErrorCode::SupplyUnderflow)?;
            // At zero junior there is no receipt to compound into. The same
            // reserved seed custody lane used for collected fees holds its half.
            end.junior_compound = Some((
                self.tranches.junior_reserve_shares,
                new_junior,
                fee_asset.code(),
                carry.swap_half_atom,
            ));
            end.junior_seed_compound = allocation.rebuild_seed;
        }
        Ok(())
    }
}

impl Market {
    /// Reprice only ownership after the enclosing swap settled a loss or used
    /// reference inventory. The trader's already executed exchange is unchanged.
    #[inline(never)]
    pub(super) fn prepare_post_settlement_compounding(
        &mut self,
        asset: MarketAsset,
        amount: u64,
    ) -> Result<super::super::liquidity::ConcentratedHlpTransition> {
        let state = self.integrated_curve_state_nad()?;
        let mut end = state;
        let price = self
            .current_concentrated_spot_price_nad()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        self.apply_compounded_tranche_fee(
            &state,
            &mut end,
            CompoundedTrancheFee {
                asset,
                amount,
                claimable_lp: 0,
                price,
            },
        )?;
        self.side_mut(asset).credit_reserve(amount, true)?;
        super::super::liquidity::prepare_concentrated_hlp_transition_from_end(self, end, false, true, 0, 0)
    }
}
