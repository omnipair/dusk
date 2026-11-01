use crate::{
    errors::ErrorCode,
    math::{mul_div_ceil_u128, mul_div_u128},
};
use anchor_lang::prelude::*;
use uint::construct_uint;

// Match the curve module's uint 0.9 compatibility scope. Rust 1.99 diagnoses
// deprecated integer APIs and a semicolon emitted by the dependency's macro.
#[allow(unknown_lints)] // The SBF compiler predates the renamed macro lint.
#[allow(
    clippy::assign_op_pattern,
    clippy::manual_div_ceil,
    deprecated,
    semicolon_in_expressions_from_non_local_macros
)]
mod wide {
    use super::construct_uint;
    construct_uint! { pub struct U256(4); }
}
use wide::U256;

const Q64: u128 = 1_u128 << 64;

/// One exposure lane's newly issued receivable units per LP receipt. The
/// weighted paid index is kept at full Q128 precision: flooring it earlier can
/// give late entrants a fractional claim on money paid before their entry.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct ReceivableGrowth {
    pub units_q64: u128,
    pub weighted_paid_q128: [u64; 4],
}

/// A bounded-denomination epoch of an unpaid-income pool. Public and hLP
/// funding income use separate books. Repayments/defaults are pooled within
/// each book; already funded advances do not remain unpaid claims.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct IncomeReceivableBook {
    pub epoch: u64,
    pub unpaid: u64,
    pub units: u64,
    pub paid_per_unit_q64: u128,
    pub paid_remainder: u64,
    /// Ordinary senior, junior, base hLP, quote hLP.
    pub growth: [ReceivableGrowth; 4],
}

impl anchor_lang::Space for Box<IncomeReceivableBook> {
    const INIT_SPACE: usize = IncomeReceivableBook::INIT_SPACE;
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct IncomeReceivableClaim {
    pub epoch: u64,
    pub units_q64: u128,
    pub paid_checkpoint_q64: u128,
    pub growth_checkpoint: ReceivableGrowth,
    pub paid_fraction_q128: u128,
    pub claimable: u64,
}

impl IncomeReceivableBook {
    /// Each lane receives an explicit gross allocation and has its own LP
    /// denominator. Allocations must conserve the newly accrued receivable.
    /// A drained or numerically exhausted epoch must be archived and rolled
    /// before issuing more units; old ownership is never silently reset.
    pub fn issue(&mut self, allocations: [(u64, u64); 4]) -> Result<[u64; 4]> {
        let amount = allocations.iter().try_fold(0_u64, |sum, (amount, _)| {
            sum.checked_add(*amount).ok_or(ErrorCode::MarketMathOverflow)
        })?;
        if amount == 0 {
            return Ok([0; 4]);
        }
        require!(self.unpaid > 0 || self.units == 0, ErrorCode::IncomeEpochNeedsRollover);
        let issued = if self.units == 0 {
            amount as u128
        } else {
            mul_div_ceil_u128(amount as u128, self.units as u128, self.unpaid as u128)?
        };
        let issued = u64::try_from(issued).map_err(|_| ErrorCode::IncomeEpochNeedsRollover)?;
        let mut next = self.clone();
        next.units = next
            .units
            .checked_add(issued)
            .ok_or(ErrorCode::IncomeEpochNeedsRollover)?;
        next.unpaid = next.unpaid.checked_add(amount).ok_or(ErrorCode::MarketMathOverflow)?;
        let mut assigned = [0_u64; 4];
        let mut cumulative_amount = 0_u128;
        let mut cumulative_units = 0_u128;
        for (i, (gross, supply)) in allocations.into_iter().enumerate() {
            cumulative_amount += gross as u128;
            let cumulative = mul_div_u128(issued as u128, cumulative_amount, amount as u128)?;
            let lane_units = cumulative - cumulative_units;
            cumulative_units = cumulative;
            assigned[i] = lane_units as u64;
            if lane_units == 0 {
                continue;
            }
            require!(supply > 0, ErrorCode::SupplyUnderflow);
            let delta = mul_div_u128(lane_units, Q64, supply as u128)?;
            let lane = &mut next.growth[i];
            lane.units_q64 = lane
                .units_q64
                .checked_add(delta)
                .ok_or(ErrorCode::IncomeEpochNeedsRollover)?;
            let weighted = U256(lane.weighted_paid_q128)
                .checked_add(
                    U256::from(delta)
                        .checked_mul(U256::from(next.paid_per_unit_q64))
                        .ok_or(ErrorCode::IncomeEpochNeedsRollover)?,
                )
                .ok_or(ErrorCode::IncomeEpochNeedsRollover)?;
            lane.weighted_paid_q128 = weighted.0;
        }
        *self = next;
        Ok(assigned)
    }

    /// Remove paid and canceled gross receivables, then allocate only measured
    /// net cash. A writeoff has zero cash. Protocol deductions and transfer fees
    /// never become holder liabilities.
    pub fn settle(&mut self, gross_removed: u64, net_cash: u64) -> Result<()> {
        require_gte!(self.unpaid, gross_removed, ErrorCode::BrokenInvariant);
        require_gte!(gross_removed, net_cash, ErrorCode::UnbackedFeeLiability);
        if gross_removed == 0 {
            return Ok(());
        }
        require!(self.units > 0, ErrorCode::BrokenInvariant);
        self.credit_cash(net_cash)?;
        self.unpaid -= gross_removed;
        Ok(())
    }

    /// Measured custody credit after an authenticated gross settlement. The
    /// market checks the pending gross budget before calling this operation.
    pub(crate) fn credit_cash(&mut self, net_cash: u64) -> Result<()> {
        if net_cash == 0 {
            return Ok(());
        }
        require!(self.units > 0, ErrorCode::BrokenInvariant);
        let scaled = (net_cash as u128)
            .checked_mul(Q64)
            .and_then(|v| v.checked_add(self.paid_remainder as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let next_paid = self
            .paid_per_unit_q64
            .checked_add(scaled / self.units as u128)
            .ok_or(ErrorCode::IncomeEpochNeedsRollover)?;
        self.paid_remainder = (scaled % self.units as u128) as u64;
        self.paid_per_unit_q64 = next_paid;
        Ok(())
    }

    /// The instruction persists the returned epoch under its immutable PDA.
    /// Holder catch-up traverses archived epochs in bounded batches; repayment
    /// never scans LP ownership history.
    pub fn roll(&mut self) -> Result<Self> {
        let old = self.clone();
        let epoch = self.epoch.checked_add(1).ok_or(ErrorCode::MarketMathOverflow)?;
        *self = Self {
            epoch,
            unpaid: old.unpaid,
            units: old.unpaid,
            ..Self::default()
        };
        Ok(old)
    }
}

impl IncomeReceivableClaim {
    /// Gross value of already-checkpointed units that borrowers have not yet
    /// repaid. This is a receivable valuation, never a spendable cash balance.
    pub fn unpaid_value(&self, book: &IncomeReceivableBook) -> Result<u64> {
        require_eq!(self.epoch, book.epoch, ErrorCode::IncomeHistoryRequired);
        if self.units_q64 == 0 || book.unpaid == 0 {
            return Ok(0);
        }
        require!(book.units > 0, ErrorCode::BrokenInvariant);
        u64::try_from(mul_div_u128(
            self.units_q64,
            book.unpaid as u128,
            (book.units as u128) << 64,
        )?)
        .map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    /// Retire a portion of this owner's receivable in exchange for funded
    /// principal. The caller must provide that backing and record the matching
    /// public-interest advance; this operation itself creates no cash credit.
    /// Whole book units are used so rounding cannot dilute the other owners.
    pub(crate) fn redeem_unpaid_units(
        &mut self,
        book: &mut IncomeReceivableBook,
        numerator: u64,
        denominator: u64,
    ) -> Result<u64> {
        require!(denominator > 0 && numerator <= denominator, ErrorCode::InvalidArgument);
        require_eq!(self.epoch, book.epoch, ErrorCode::IncomeHistoryRequired);
        require_eq!(
            self.paid_checkpoint_q64,
            book.paid_per_unit_q64,
            ErrorCode::BrokenInvariant
        );
        if numerator == 0 || self.units_q64 == 0 || book.unpaid == 0 {
            return Ok(0);
        }
        let units = u64::try_from(mul_div_u128(self.units_q64, numerator as u128, denominator as u128)? >> 64)
            .map_err(|_| ErrorCode::MarketMathOverflow)?;
        require_gte!(book.units, units, ErrorCode::BrokenInvariant);
        let value = u64::try_from(mul_div_u128(units as u128, book.unpaid as u128, book.units as u128)?)
            .map_err(|_| ErrorCode::MarketMathOverflow)?;
        if value == 0 {
            return Ok(0);
        }
        self.units_q64 -= (units as u128) << 64;
        book.units -= units;
        book.unpaid -= value;
        Ok(value)
    }

    pub fn checkpoint_exposure(
        &mut self,
        balance: u64,
        book: &IncomeReceivableBook,
        lane: usize,
        end: super::IncomeExposureEnd,
    ) -> Result<()> {
        if !end.ended || book.epoch < end.epoch {
            return self.checkpoint(balance, book, lane);
        }
        if book.epoch > end.epoch {
            return self.checkpoint(0, book, lane);
        }
        let mut closed = book.clone();
        closed.growth[lane] = end.growth;
        self.checkpoint(balance, &closed, lane)
    }

    pub fn advance_exposure_epoch(
        &mut self,
        balance: u64,
        book: &IncomeReceivableBook,
        lane: usize,
        end: super::IncomeExposureEnd,
    ) -> Result<()> {
        if !end.ended || book.epoch < end.epoch {
            return self.advance_epoch(balance, book, lane);
        }
        if book.epoch > end.epoch {
            return self.advance_epoch(0, book, lane);
        }
        let mut closed = book.clone();
        closed.growth[lane] = end.growth;
        self.advance_epoch(balance, &closed, lane)
    }

    pub fn checkpoint(&mut self, balance: u64, book: &IncomeReceivableBook, lane: usize) -> Result<()> {
        require_eq!(self.epoch, book.epoch, ErrorCode::IncomeHistoryRequired);
        let growth = book.growth.get(lane).ok_or(ErrorCode::InvalidArgument)?;
        if *growth == self.growth_checkpoint && book.paid_per_unit_q64 == self.paid_checkpoint_q64 {
            return Ok(());
        }
        let delta = growth
            .units_q64
            .checked_sub(self.growth_checkpoint.units_q64)
            .ok_or(ErrorCode::BrokenInvariant)?;
        let weighted = U256(growth.weighted_paid_q128)
            .checked_sub(U256(self.growth_checkpoint.weighted_paid_q128))
            .ok_or(ErrorCode::BrokenInvariant)?;
        let new_unit_income = U256::from(delta)
            .checked_mul(U256::from(book.paid_per_unit_q64))
            .and_then(|v| v.checked_sub(weighted))
            .ok_or(ErrorCode::BrokenInvariant)?;
        let old_unit_income = U256::from(self.units_q64)
            .checked_mul(U256::from(
                book.paid_per_unit_q64
                    .checked_sub(self.paid_checkpoint_q64)
                    .ok_or(ErrorCode::BrokenInvariant)?,
            ))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let cash_scaled = new_unit_income
            .checked_mul(U256::from(balance))
            .and_then(|v| v.checked_add(old_unit_income))
            .and_then(|v| v.checked_add(U256::from(self.paid_fraction_q128)))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let whole = cash_scaled >> 128;
        require!(whole <= U256::from(u64::MAX), ErrorCode::MarketMathOverflow);
        let units = mul_div_u128(balance as u128, delta, 1)?
            .checked_add(self.units_q64)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let claimable = self
            .claimable
            .checked_add(whole.as_u64())
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.units_q64 = units;
        self.claimable = claimable;
        self.paid_fraction_q128 = cash_scaled.low_u128();
        self.paid_checkpoint_q64 = book.paid_per_unit_q64;
        self.growth_checkpoint = *growth;
        Ok(())
    }

    pub fn advance_epoch(&mut self, balance: u64, archived: &IncomeReceivableBook, lane: usize) -> Result<()> {
        self.checkpoint(balance, archived, lane)?;
        self.units_q64 = if archived.units == 0 {
            0
        } else {
            mul_div_u128(self.units_q64, archived.unpaid as u128, archived.units as u128)?
        };
        self.epoch = self.epoch.checked_add(1).ok_or(ErrorCode::MarketMathOverflow)?;
        self.growth_checkpoint = ReceivableGrowth::default();
        self.paid_checkpoint_q64 = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/transitions/tranches/income_receivable.rs");
}
