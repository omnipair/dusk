use crate::{errors::ErrorCode, state::TrancheCapital};
use anchor_lang::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrancheRevenueSource {
    /// All ordinary, leverage and hLP funding interest after existing cuts.
    BorrowInterest,
    /// LP-directed swap revenue after protocol cuts and measured transfer fees.
    SwapFee,
    /// The junior underwriting portion of a liquidation allocation.
    LiquidationCoverage,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrancheRevenueAllocation {
    pub senior: u64,
    pub junior: u64,
    /// New funded capital for the permanent owner when junior has no backing.
    /// Never publish this as income on a wiped generation's growth index.
    pub rebuild_seed: u64,
}

/// Carry sub-basis-point atoms across credits so splitting a payment into many
/// small transfers cannot change either tranche's governed allocation.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct TrancheRevenueCarry {
    pub interest_remainder_bps: u16,
    pub swap_remainder_bps: u16,
}

impl TrancheRevenueCarry {
    pub fn allocate(
        &mut self,
        net_lp_credit: u64,
        source: TrancheRevenueSource,
        capital: &TrancheCapital,
        junior_income_share_bps: u16,
    ) -> Result<TrancheRevenueAllocation> {
        require!(
            capital.junior_reserve_shares == 0 || capital.junior_receipt_supply > 0,
            ErrorCode::BrokenInvariant
        );
        let junior = match source {
            TrancheRevenueSource::LiquidationCoverage => net_lp_credit,
            TrancheRevenueSource::BorrowInterest | TrancheRevenueSource::SwapFee => {
                let carry = match source {
                    TrancheRevenueSource::BorrowInterest => &mut self.interest_remainder_bps,
                    _ => &mut self.swap_remainder_bps,
                };
                require!(junior_income_share_bps <= 10_000, ErrorCode::InvalidParameterUpdate);
                let numerator = (net_lp_credit as u128)
                    .checked_mul(u128::from(junior_income_share_bps))
                    .and_then(|value| value.checked_add(u128::from(*carry)))
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                *carry = u16::try_from(numerator % 10_000).map_err(|_| ErrorCode::MarketMathOverflow)?;
                u64::try_from(numerator / 10_000).map_err(|_| ErrorCode::MarketMathOverflow)?
            }
        };
        let mut allocation = TrancheRevenueAllocation {
            senior: net_lp_credit - junior,
            ..Default::default()
        };
        if capital.junior_reserve_shares == 0 {
            allocation.rebuild_seed = junior;
        } else {
            allocation.junior = junior;
        }
        Ok(allocation)
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/transitions/tranches/tranche_revenue.rs");
}
