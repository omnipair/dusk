use anchor_lang::prelude::*;

use super::MarketConfig;
use crate::errors::ErrorCode;
use crate::math::{directional_ema_u64, ema_u128, ema_u64, observed_or_current_u64};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct Risk {
    pub base_price_ema_nad: u64,
    pub quote_price_ema_nad: u64,
    pub directional_base_price_ema_nad: u64,
    pub directional_quote_price_ema_nad: u64,
    pub cached_spot_base_price_nad: u64,
    pub cached_spot_quote_price_nad: u64,
    /// Last observed balanced-equivalent CONCENTRATED depth.
    pub cached_q_nad: u128,
    /// EMA of balanced-equivalent CONCENTRATED depth. This replaces the CPMM `K` EMA
    /// while retaining the same serialized width.
    pub q_ema_nad: u128,
    pub last_snapshot_slot: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct MarketHealth {
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub effective_base_debt_nad: u128,
    pub effective_quote_debt_nad: u128,
    pub base_debt_health_bps: u64,
    pub quote_debt_health_bps: u64,
}

impl Risk {
    pub const fn conservative_q_nad(&self) -> u128 {
        if self.q_ema_nad == 0 {
            self.cached_q_nad
        } else if self.cached_q_nad == 0 {
            self.q_ema_nad
        } else if self.cached_q_nad < self.q_ema_nad {
            self.cached_q_nad
        } else {
            self.q_ema_nad
        }
    }

    pub fn refreshed(
        &self,
        current_base_price_nad: u64,
        current_quote_price_nad: u64,
        current_q_nad: u128,
        config: &MarketConfig,
        current_slot: u64,
    ) -> Result<Self> {
        require!(
            current_base_price_nad > 0 && current_quote_price_nad > 0 && current_q_nad > 0,
            ErrorCode::InsufficientLiquidity
        );

        let cached_spot_base_price_nad =
            observed_or_current_u64(self.cached_spot_base_price_nad, current_base_price_nad);
        let cached_spot_quote_price_nad =
            observed_or_current_u64(self.cached_spot_quote_price_nad, current_quote_price_nad);
        let cached_q_nad = if self.cached_q_nad == 0 {
            current_q_nad
        } else {
            self.cached_q_nad
        };

        let base_price_ema_nad = ema_u64(
            self.base_price_ema_nad,
            cached_spot_base_price_nad,
            self.last_snapshot_slot,
            current_slot,
            config.ema_half_life_ms,
        );
        let quote_price_ema_nad = ema_u64(
            self.quote_price_ema_nad,
            cached_spot_quote_price_nad,
            self.last_snapshot_slot,
            current_slot,
            config.ema_half_life_ms,
        );
        let directional_base_price_ema_nad = directional_ema_u64(
            self.directional_base_price_ema_nad,
            cached_spot_base_price_nad,
            self.last_snapshot_slot,
            current_slot,
            config.directional_ema_half_life_ms,
        );
        let directional_quote_price_ema_nad = directional_ema_u64(
            self.directional_quote_price_ema_nad,
            cached_spot_quote_price_nad,
            self.last_snapshot_slot,
            current_slot,
            config.directional_ema_half_life_ms,
        );
        let q_ema_nad = ema_u128(
            self.q_ema_nad,
            cached_q_nad,
            self.last_snapshot_slot,
            current_slot,
            config.q_ema_half_life_ms,
        );

        Ok(Self {
            base_price_ema_nad,
            quote_price_ema_nad,
            directional_base_price_ema_nad,
            directional_quote_price_ema_nad,
            cached_spot_base_price_nad: current_base_price_nad,
            cached_spot_quote_price_nad: current_quote_price_nad,
            cached_q_nad: current_q_nad,
            q_ema_nad,
            last_snapshot_slot: current_slot,
        })
    }
}
