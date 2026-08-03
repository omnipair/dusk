use anchor_lang::prelude::*;

use super::{AmmCurveParameters, MarketConfig, RiskCurveCache, RiskCurveReserves};
use crate::constants::NAD;
use crate::math::{
    concentrated_risk_reserves_at_price_q, directional_ema_u64, ema_u128, ema_u64, observed_or_current_u128,
    observed_or_current_u64, ConcentratedRiskReserves, ConcentratedSwapDirection,
};
use crate::{errors::ErrorCode, shared::math::ceil_div};

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static RISK_SHAPE_RECONSTRUCTIONS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn reset_risk_shape_reconstructions() {
    RISK_SHAPE_RECONSTRUCTIONS.with(|count| count.set(0));
}

#[cfg(test)]
fn risk_shape_reconstructions() -> usize {
    RISK_SHAPE_RECONSTRUCTIONS.with(Cell::get)
}

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
        let cached_q_nad = observed_or_current_u128(self.cached_q_nad, current_q_nad);

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

fn canonical_base_price_for_quote_collateral(quote_price_nad: u64) -> Result<u128> {
    require!(quote_price_nad > 0, ErrorCode::InvalidSettlementPrice);
    ceil_div(
        (NAD as u128)
            .checked_mul(NAD as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        quote_price_nad as u128,
    )
    .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn cached_shape(
    target_base_price_nad: u128,
    q_ema_nad: u128,
    collateral_to_debt: ConcentratedSwapDirection,
    center_price_nad: u64,
    parameters: AmmCurveParameters,
) -> Result<RiskCurveReserves> {
    #[cfg(test)]
    RISK_SHAPE_RECONSTRUCTIONS.with(|count| count.set(count.get() + 1));

    let ConcentratedRiskReserves {
        base_reserve_nad,
        quote_reserve_nad,
    } = concentrated_risk_reserves_at_price_q(
        target_base_price_nad,
        q_ema_nad,
        collateral_to_debt,
        center_price_nad as u128,
        parameters.peak_depth_nad as u128,
        parameters.imbalance_scale_nad as u128,
    )?;
    Ok(RiskCurveReserves {
        base_reserve_nad,
        quote_reserve_nad,
    })
}

/// Compares only coordinates that determine the four reconstructed reserve
/// shapes. Spot observations and snapshot slots may advance without changing
/// those shapes, so they must not force an expensive cache rebuild.
pub(super) fn cache_inputs_match(left: &Risk, right: &Risk) -> bool {
    left.conservative_q_nad() == right.conservative_q_nad()
        && left.base_price_ema_nad == right.base_price_ema_nad
        && left.base_price_ema_nad.min(left.directional_base_price_ema_nad)
            == right.base_price_ema_nad.min(right.directional_base_price_ema_nad)
        && left.quote_price_ema_nad == right.quote_price_ema_nad
        && left.quote_price_ema_nad.min(left.directional_quote_price_ema_nad)
            == right.quote_price_ema_nad.min(right.directional_quote_price_ema_nad)
}

impl RiskCurveCache {
    /// Builds four separately conservative shapes. Base and quote EMAs are
    /// intentionally not assumed to be exact reciprocals.
    pub(crate) fn from_risk(risk: &Risk, center_price_nad: u64, parameters: AmmCurveParameters) -> Result<Self> {
        require!(
            risk.base_price_ema_nad > 0
                && risk.quote_price_ema_nad > 0
                && risk.directional_base_price_ema_nad > 0
                && risk.directional_quote_price_ema_nad > 0
                && risk.conservative_q_nad() > 0,
            ErrorCode::InsufficientLiquidity
        );
        let conservative_q_nad = risk.conservative_q_nad();

        let base_liquidation_price = risk.base_price_ema_nad as u128;
        let base_underwriting_price = risk.base_price_ema_nad.min(risk.directional_base_price_ema_nad) as u128;
        let quote_liquidation_price = canonical_base_price_for_quote_collateral(risk.quote_price_ema_nad)?;
        let quote_underwriting_price = canonical_base_price_for_quote_collateral(
            risk.quote_price_ema_nad.min(risk.directional_quote_price_ema_nad),
        )?;

        let base_liquidation = cached_shape(
            base_liquidation_price,
            conservative_q_nad,
            ConcentratedSwapDirection::BaseToQuote,
            center_price_nad,
            parameters,
        )?;
        let base_underwriting = if base_underwriting_price == base_liquidation_price {
            base_liquidation
        } else {
            cached_shape(
                base_underwriting_price,
                conservative_q_nad,
                ConcentratedSwapDirection::BaseToQuote,
                center_price_nad,
                parameters,
            )?
        };
        let quote_liquidation = cached_shape(
            quote_liquidation_price,
            conservative_q_nad,
            ConcentratedSwapDirection::QuoteToBase,
            center_price_nad,
            parameters,
        )?;
        let quote_underwriting = if quote_underwriting_price == quote_liquidation_price {
            quote_liquidation
        } else {
            cached_shape(
                quote_underwriting_price,
                conservative_q_nad,
                ConcentratedSwapDirection::QuoteToBase,
                center_price_nad,
                parameters,
            )?
        };

        Ok(Self {
            base_underwriting,
            quote_underwriting,
            base_liquidation,
            quote_liquidation,
            center_price_nad,
            peak_depth_nad: parameters.peak_depth_nad,
            imbalance_scale_nad: parameters.imbalance_scale_nad,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_risk() -> Risk {
        Risk {
            base_price_ema_nad: NAD,
            quote_price_ema_nad: NAD,
            directional_base_price_ema_nad: NAD,
            directional_quote_price_ema_nad: NAD,
            cached_spot_base_price_nad: NAD,
            cached_spot_quote_price_nad: NAD,
            cached_q_nad: 1_000_000_u128 * NAD as u128,
            q_ema_nad: 1_000_000_u128 * NAD as u128,
            last_snapshot_slot: 10,
        }
    }

    #[test]
    fn cache_match_ignores_snapshot_metadata_but_not_shape_inputs() {
        let risk = sample_risk();
        let mut metadata_only = risk;
        metadata_only.cached_spot_base_price_nad = NAD * 2;
        metadata_only.cached_spot_quote_price_nad = NAD / 2;
        metadata_only.last_snapshot_slot += 1;
        assert!(cache_inputs_match(&risk, &metadata_only));

        let mut changed_q = risk;
        changed_q.cached_q_nad /= 2;
        assert!(!cache_inputs_match(&risk, &changed_q));

        let mut changed_underwriting = risk;
        changed_underwriting.directional_base_price_ema_nad = NAD / 2;
        assert!(!cache_inputs_match(&risk, &changed_underwriting));

        let mut non_binding_directional = risk;
        non_binding_directional.directional_base_price_ema_nad = NAD * 2;
        assert!(cache_inputs_match(&risk, &non_binding_directional));
    }

    #[test]
    fn cache_builder_reuses_equal_underwriting_and_liquidation_shapes() {
        let risk = sample_risk();
        let parameters = AmmCurveParameters {
            peak_depth_nad: 200 * NAD,
            imbalance_scale_nad: NAD / 10,
        };

        reset_risk_shape_reconstructions();
        RiskCurveCache::from_risk(&risk, NAD, parameters).unwrap();
        assert_eq!(risk_shape_reconstructions(), 2);

        let distinct = Risk {
            directional_base_price_ema_nad: NAD / 2,
            directional_quote_price_ema_nad: NAD / 2,
            ..risk
        };
        reset_risk_shape_reconstructions();
        RiskCurveCache::from_risk(&distinct, NAD, parameters).unwrap();
        assert_eq!(risk_shape_reconstructions(), 4);
    }
}
