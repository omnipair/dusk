use super::swap::InventorySettlement;
use crate::{
    constants::NAD,
    errors::ErrorCode,
    state::*,
    transitions::{
        amm::{prepare_concentrated_cache_at_depth, PreparedSwap, SwapRequest},
        liquidity::{prepare_concentrated_hlp_transition_at_current_state, IntegratedCurveState, SwapCashPolicy},
        HlpRebalanceReceipt, HlpYieldEligibility,
    },
};
use anchor_lang::prelude::*;

impl Market {
    // Only the oracle price/depth and configured shape define these coordinates.
    // hLP cash, claims and debt belong to real settlement, never this quote.
    #[inline(never)]
    pub(crate) fn ema_liquidation_curve(
        &self,
        collateral_asset: MarketAsset,
    ) -> Result<(Box<Market>, IntegratedCurveState)> {
        let risk = self.current_risk()?;
        let collateral_price = self.pessimistic_collateral_price_nad(collateral_asset, &risk, false);
        require_gt!(collateral_price, 0, ErrorCode::InvalidSettlementPrice);
        // The two symmetric EMAs are averaged independently; they are not
        // generally reciprocals. Use the same collateral direction as health.
        let price = if collateral_asset == MarketAsset::Base {
            collateral_price
        } else {
            u64::try_from(u128::from(NAD).pow(2) / u128::from(collateral_price))
                .map_err(|_| ErrorCode::MarketMathOverflow)?
        };
        let depth = risk.curve_depth_ema_nad;
        let cache =
            prepare_concentrated_cache_at_depth(price, depth, self.config.amm.concentrated_curve_parameters()?)?;
        let point = cache.center_point_with_geometry(price, cache.geometry()?)?;
        let mut reference = self.liquidation_scratch();
        reference.amm.initialized = true;
        reference.amm.center_price_nad = price;
        reference.amm.concentrated_curve_cache = cache;
        // These shares only affect the discarded virtual fee-compounding end.
        // The real prepared swap separately captures all fee/yield ownership.
        reference.base_hlp_vault.ylp_shares = 0;
        reference.quote_hlp_vault.ylp_shares = 0;
        let state = IntegratedCurveState {
            ordinary_base: point.base_reserve,
            ordinary_quote: point.quote_reserve,
            base_hlp_equity: 0,
            quote_hlp_equity: 0,
            base_hlp_quote_debt: 0,
            quote_hlp_base_debt: 0,
            ..IntegratedCurveState::default()
        };
        Ok((reference, state))
    }
}

impl SwapRequest {
    // This is an inventory settlement. Its virtual hLP endpoint/cache must
    // never be applied to the real pool. PreparedSwap enforces that distinction.
    pub(crate) fn prepare_ema_liquidation(self, market: &mut Market) -> Result<Box<PreparedSwap>> {
        market.accrue_interest_to_clock(crate::clock::MarketClock::from_timestamp(
            self.current_slot,
            self.current_unix_timestamp,
        )?)?;
        market.prepare_amm_for_swap(crate::clock::MarketClock::from_timestamp(
            self.current_slot,
            self.current_unix_timestamp,
        )?)?;
        let (reference, state) = market.ema_liquidation_curve(self.asset_in)?;
        let preliminary = reference.preliminary_swap_inputs_at_reference_price(
            self.asset_in,
            self.reserve_credit,
            crate::clock::MarketClock::from_timestamp(self.current_slot, self.current_unix_timestamp)?,
            self.current_unix_timestamp,
            reference.dynamic_fee_pre_state(crate::clock::MarketClock::from_timestamp(
                self.current_slot,
                self.current_unix_timestamp,
            )?)?,
            reference.amm.center_price_nad,
        )?;
        let quote = reference
            .quote_concentrated_integrated_with_fee_from_state(
                self.asset_in,
                self.reserve_credit,
                preliminary,
                state,
                self.protocol_fee_bps,
            )?
            .ok_or(ErrorCode::BrokenInvariant)?
            .as_swap_quote(self.asset_in);
        let eligibility = HlpYieldEligibility {
            ylp_supply: market.base_side.shares.ylp_supply,
            base_hlp_ylp_shares: market.base_hlp_vault.ylp_shares,
            quote_hlp_ylp_shares: market.quote_hlp_vault.ylp_shares,
        };
        Ok(Box::new(PreparedSwap {
            quoted_slot: self.current_slot,
            quote,
            base_pre_rebalance: HlpRebalanceReceipt {
                target_asset: MarketAsset::Base,
                ..Default::default()
            },
            quote_pre_rebalance: HlpRebalanceReceipt {
                target_asset: MarketAsset::Quote,
                ..Default::default()
            },
            fee_eligible_ylp_supply: eligibility.ylp_supply,
            interest_eligibility: eligibility,
            cash_policy: SwapCashPolicy::LiquidationQuote,
            post_fee_curve_cache: None,
            // Real funding cash floors only; apply reconstructs after settlement.
            concentrated_transition: Some(Box::new(prepare_concentrated_hlp_transition_at_current_state(market)?)),
            inventory_settlement: Some(InventorySettlement {
                curve_revision: market.curve_revision,
                actual_start_price_nad: market
                    .current_concentrated_spot_price_nad()?
                    .ok_or(ErrorCode::BrokenInvariant)?,
            }),
        }))
    }
}
