use crate::{errors::ErrorCode, state::TrancheCapital};
use anchor_lang::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrancheRevenueSource {
    /// All ordinary, leverage and hLP funding interest after existing cuts.
    BorrowInterest,
    /// LP-directed swap revenue after protocol cuts and measured transfer fees.
    SwapFee,
    /// Only the former insurance-fill portion of a liquidation allocation.
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

/// Carry the indivisible half atom across credits so splitting a payment into
/// many small transfers cannot eliminate one tranche's entire allocation.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct TrancheRevenueCarry {
    pub interest_half_atom: bool,
    pub swap_half_atom: bool,
}

impl TrancheRevenueCarry {
    pub fn allocate(
        &mut self,
        net_lp_credit: u64,
        source: TrancheRevenueSource,
        capital: &TrancheCapital,
    ) -> Result<TrancheRevenueAllocation> {
        require!(
            capital.junior_reserve_shares == 0 || capital.junior_receipt_supply > 0,
            ErrorCode::BrokenInvariant
        );
        let junior = match source {
            TrancheRevenueSource::LiquidationCoverage => net_lp_credit,
            TrancheRevenueSource::BorrowInterest | TrancheRevenueSource::SwapFee => {
                let carry = match source {
                    TrancheRevenueSource::BorrowInterest => &mut self.interest_half_atom,
                    _ => &mut self.swap_half_atom,
                };
                let numerator = net_lp_credit as u128 + u128::from(*carry);
                *carry = numerator % 2 == 1;
                (numerator / 2) as u64
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
