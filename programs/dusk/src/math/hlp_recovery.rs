//! O(1) hLP funding-recovery pricing.
//!
//! Recovery is priced from the vault's economic imbalance, not from a keeper
//! clock. A healthy hLP has indexed funding debt equal to its opposite-asset
//! yLP claim. When debt grows above that claim, a swap supplying the borrowed
//! asset may receive a target-asset bonus funded solely by that hLP's equity.

use anchor_lang::prelude::*;

use crate::{constants::BPS_DENOMINATOR, errors::ErrorCode};

/// Yield-Basis-like relative stress bands. The canonical opposite claim is
/// 1.0; normal recovery reaches its maximum incentive at 17/16 and the
/// terminal-risk boundary is 9/8.
pub(crate) const HLP_RECOVERY_SAFE_DENOMINATOR: u128 = 16;
pub(crate) const HLP_RECOVERY_CRITICAL_NUMERATOR: u128 = 9;
pub(crate) const HLP_RECOVERY_CRITICAL_DENOMINATOR: u128 = 8;
/// Ignore dust funding drift below 25 bps. It remains visible in preview but
/// does not make an ordinary swap pay recovery arithmetic or alter hLP state.
pub(crate) const HLP_RECOVERY_MIN_STRESS_BPS: u128 = 25;
pub(crate) const HLP_RECOVERY_MAX_DISCOUNT_BPS: u16 = 500;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HlpRecoveryQuote {
    /// Borrowed-asset debt in excess of the canonical opposite claim after
    /// revenue which is already legally available for netting.
    pub funding_gap: u64,
    /// Portion of this swap's borrowed-asset input which caps the incentive.
    /// The input still follows the ordinary curve.
    pub matched_input: u64,
    /// Additional target-asset output funded by burning hLP equity.
    pub bonus_output: u64,
    pub discount_bps: u16,
    pub critical: bool,
}

/// Prices the recovery bonus for a swap supplying the hLP's borrowed asset.
///
/// `target_per_opposite_num / target_per_opposite_den` is the current ordinary
/// reserve conversion into the hLP target asset. All divisions floor, so the
/// trader bonus cannot consume more hLP equity than the quoted amount.
pub(crate) fn quote_hlp_recovery(
    actual_debt: u64,
    canonical_opposite_claim: u64,
    usable_revenue: u64,
    matching_input: u64,
    target_equity: u64,
    target_per_opposite_num: u64,
    target_per_opposite_den: u64,
) -> Result<HlpRecoveryQuote> {
    let debt_after_revenue = actual_debt.saturating_sub(usable_revenue);
    let funding_gap = debt_after_revenue.saturating_sub(canonical_opposite_claim);
    let critical = if canonical_opposite_claim == 0 {
        actual_debt > 0
    } else {
        (actual_debt as u128) * HLP_RECOVERY_CRITICAL_DENOMINATOR
            >= (canonical_opposite_claim as u128) * HLP_RECOVERY_CRITICAL_NUMERATOR
    };
    if canonical_opposite_claim == 0
        || funding_gap == 0
        || target_equity == 0
        || matching_input == 0
        || target_per_opposite_num == 0
        || target_per_opposite_den == 0
    {
        return Ok(HlpRecoveryQuote {
            funding_gap,
            critical,
            ..HlpRecoveryQuote::default()
        });
    }
    if (funding_gap as u128) * (BPS_DENOMINATOR as u128)
        < (canonical_opposite_claim as u128) * HLP_RECOVERY_MIN_STRESS_BPS
    {
        return Ok(HlpRecoveryQuote {
            funding_gap,
            critical,
            ..HlpRecoveryQuote::default()
        });
    }

    // Progress is linear from the canonical point to 17/16. Multiplying the
    // gap by 16 avoids first flooring claim/16 to zero on small positions.
    let discount_bps_u128 = ((funding_gap as u128)
        .checked_mul(
            HLP_RECOVERY_SAFE_DENOMINATOR
                .checked_mul(HLP_RECOVERY_MAX_DISCOUNT_BPS as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )
        .ok_or(ErrorCode::MarketMathOverflow)?
        / canonical_opposite_claim as u128)
        .min(HLP_RECOVERY_MAX_DISCOUNT_BPS as u128);
    let discount_bps = u16::try_from(discount_bps_u128).map_err(|_| ErrorCode::MarketMathOverflow)?;
    if discount_bps == 0 {
        return Ok(HlpRecoveryQuote {
            funding_gap,
            critical,
            ..HlpRecoveryQuote::default()
        });
    }

    let matched_input = funding_gap.min(matching_input);
    let fair_target_output = u64::try_from(
        (matched_input as u128)
            .checked_mul(target_per_opposite_num as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?
            / target_per_opposite_den as u128,
    )
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    // A d% discount means the trader receives fair/(1-d). The incremental
    // bonus is therefore fair*d/(1-d), not fair*d.
    let discount_denominator = (BPS_DENOMINATOR as u128)
        .checked_sub(discount_bps as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let bonus_output = u64::try_from(
        (fair_target_output as u128)
            .checked_mul(discount_bps as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?
            / discount_denominator,
    )
    .map_err(|_| ErrorCode::MarketMathOverflow)?
    .min(target_equity);

    Ok(HlpRecoveryQuote {
        funding_gap,
        matched_input,
        bonus_output,
        discount_bps,
        critical,
    })
}

#[cfg(test)]
mod tests {
    include!("../tests/math/hlp_recovery.rs");
}
