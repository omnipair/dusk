use crate::{
    errors::ErrorCode,
    math::mul_div_u128,
    state::{IncomeExposureEnd, IncomeReceivableBook, IncomeReceivableClaim, ReceivableGrowth},
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

/// Per-unit value credited to a unit of a drained epoch that is advanced in
/// place, in Q64. A loss-free epoch issues every unit at no more than its
/// share of the pool (issuance rounds down), so its paid index reaches at
/// least 1.0 minus the paid-index flooring. The margin keeps catch-up value at
/// or below the cash actually credited.
const DRAINED_UNIT_VALUE_Q64: u128 = Q64 - (1_u128 << 32);

impl anchor_lang::Space for Box<IncomeReceivableBook> {
    const INIT_SPACE: usize = IncomeReceivableBook::INIT_SPACE;
}

impl IncomeReceivableBook {
    /// Each lane receives an explicit gross allocation and has its own LP
    /// denominator. Allocations must conserve the newly accrued receivable.
    /// A loss-free epoch whose receivable is fully repaid advances in place
    /// before issuing; holders catch up across it without archived history.
    /// An impaired or numerically exhausted epoch must be archived and rolled
    /// before issuing more units; old ownership is never silently reset.
    pub fn issue(&mut self, allocations: [(u64, u64); 4]) -> Result<[u64; 4]> {
        let (next, assigned) = self
            .project_issue(allocations)?
            .ok_or(ErrorCode::IncomeEpochNeedsRollover)?;
        *self = next;
        Ok(assigned)
    }

    pub(crate) fn project_issue(&self, allocations: [(u64, u64); 4]) -> Result<Option<(Self, [u64; 4])>> {
        let amount = allocations.iter().try_fold(0_u64, |sum, (amount, _)| {
            sum.checked_add(*amount).ok_or(ErrorCode::MarketMathOverflow)
        })?;
        if amount == 0 {
            return Ok(Some((self.clone(), [0; 4])));
        }
        if self.unpaid == 0 && self.units != 0 {
            if !self.can_advance_in_place() {
                return Ok(None);
            }
            // Start the next epoch of this drain run without an archive.
            // Growth stays cumulative so holders value the skipped epochs.
            let next = Self {
                epoch: self.epoch.checked_add(1).ok_or(ErrorCode::MarketMathOverflow)?,
                growth: self.growth,
                run_start_epoch: self.run_start_epoch,
                epoch_start_growth: [self.growth[0], self.growth[1]],
                ..Self::default()
            };
            return next.project_issue(allocations);
        }
        // Rounding down never issues a unit above its share of the pool, so a
        // loss-free epoch keeps paid plus unpaid value per unit at or above
        // 1.0, which in-place advancement relies on. u64 factors fit u128.
        let issued = if self.units == 0 {
            amount as u128
        } else {
            amount as u128 * self.units as u128 / self.unpaid as u128
        };
        let Ok(issued) = u64::try_from(issued) else {
            return Ok(None);
        };
        let (Some(units), Some(unpaid)) = (self.units.checked_add(issued), self.unpaid.checked_add(amount)) else {
            return Ok(None);
        };
        let mut next = self.clone();
        next.units = units;
        next.unpaid = unpaid;
        let mut assigned = [0_u64; 4];
        let mut cumulative_amount = 0_u128;
        let mut cumulative_units = 0_u128;
        for (i, (gross, supply)) in allocations.into_iter().enumerate() {
            cumulative_amount += gross as u128;
            let cumulative = issued as u128 * cumulative_amount / amount as u128;
            let lane_units = cumulative - cumulative_units;
            cumulative_units = cumulative;
            assigned[i] = lane_units as u64;
            if lane_units == 0 {
                continue;
            }
            require!(supply > 0, ErrorCode::SupplyUnderflow);
            let delta = lane_units * Q64 / supply as u128;
            let lane = &mut next.growth[i];
            let Some(growth) = lane.units_q64.checked_add(delta) else {
                return Ok(None);
            };
            let weighted_delta = U256::from(delta) * U256::from(next.paid_per_unit_q64);
            let Some(weighted) = U256(lane.weighted_paid_q128).checked_add(weighted_delta) else {
                return Ok(None);
            };
            lane.units_q64 = growth;
            lane.weighted_paid_q128 = weighted.0;
        }
        Ok(Some((next, assigned)))
    }

    /// Remove paid and canceled gross receivables, then allocate only measured
    /// net cash. A writeoff has zero cash. Protocol deductions and transfer fees
    /// never become holder liabilities. The market marks the epoch impaired
    /// when the measured cash falls short of the gross removed.
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

    /// Whether `roll_income_epoch` may archive this book. A roll makes every
    /// srLP and junior holder run income catch-up before their next balance
    /// change, so it needs real progress: at least half of the epoch's units
    /// repaid, or a book close to its numeric limits. The second arm keeps
    /// recovery possible after a small repayment followed by a large accrual.
    pub fn may_roll(&self) -> bool {
        if self.unpaid == 0 && self.units != 0 && self.can_advance_in_place() && !self.near_capacity() {
            // The next issuance advances this epoch without an archive. Lane
            // growth stays cumulative across a run, so a run near its limits
            // still needs an archived roll to restart it.
            return false;
        }
        self.units > self.unpaid && (self.unpaid <= self.units / 2 || self.near_capacity())
    }

    /// A fully repaid, loss-free epoch whose paid index reached the drained
    /// unit value. Its holders are owed exactly that value per unit, so they
    /// can catch up from cumulative lane growth alone.
    pub fn can_advance_in_place(&self) -> bool {
        self.unpaid == 0 && !self.impaired && self.paid_per_unit_q64 >= DRAINED_UNIT_VALUE_Q64
    }

    pub(crate) fn mark_impaired(&mut self) {
        self.impaired = true;
    }

    /// Within a factor of two of a fixed-width limit that would make further
    /// issuance or cash credit fail with `IncomeEpochNeedsRollover`.
    pub fn near_capacity(&self) -> bool {
        const HALF_U64: u64 = u64::MAX / 2;
        const HALF_U128: u128 = u128::MAX / 2;
        self.units >= HALF_U64
            || self.unpaid >= HALF_U64
            || self.paid_per_unit_q64 >= HALF_U128
            || self
                .growth
                .iter()
                .any(|lane| lane.units_q64 >= HALF_U128 || lane.weighted_paid_q128[3] >= HALF_U64)
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
            run_start_epoch: epoch,
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
        end: IncomeExposureEnd,
    ) -> Result<()> {
        self.catch_up_drained(balance, book, lane, end)?;
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
        end: IncomeExposureEnd,
    ) -> Result<()> {
        self.catch_up_drained(balance, book, lane, end)?;
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

    /// Value epochs of the current drain run that ended after this claim's
    /// checkpoint. Each was fully repaid without loss, so every unit in it
    /// received at least `DRAINED_UNIT_VALUE_Q64` per unit, and no unit
    /// carried over. Units held at the checkpoint are owed the rest of that
    /// value; units issued since are valued from cumulative lane growth up to
    /// the current epoch's start (or the exposure end, if earlier). This is
    /// at most the cash credited, and needs no archived epoch.
    fn catch_up_drained(
        &mut self,
        balance: u64,
        book: &IncomeReceivableBook,
        lane: usize,
        end: IncomeExposureEnd,
    ) -> Result<()> {
        if self.epoch == book.epoch {
            return Ok(());
        }
        require!(
            self.epoch < book.epoch && self.epoch >= book.run_start_epoch,
            ErrorCode::IncomeHistoryRequired
        );
        require!(lane < 4, ErrorCode::InvalidArgument);
        let start = match book.epoch_start_growth.get(lane) {
            Some(start) => *start,
            None => {
                // An hLP lane's only claim is checkpointed after every
                // issuance, and an epoch advances in place only inside an
                // issuance, so it lags exactly one epoch and its checkpoint
                // is that epoch's starting growth.
                require!(
                    self.epoch.checked_add(1) == Some(book.epoch),
                    ErrorCode::IncomeHistoryRequired
                );
                self.growth_checkpoint
            }
        };
        let stop = if end.ended && end.epoch < book.epoch {
            (end.epoch >= self.epoch).then_some(end.growth)
        } else {
            Some(start)
        }
        .filter(|stop| stop.units_q64 >= self.growth_checkpoint.units_q64);
        let drained_value = |growth: &ReceivableGrowth| {
            (U256::from(growth.units_q64) * U256::from(DRAINED_UNIT_VALUE_Q64))
                .overflowing_sub(U256(growth.weighted_paid_q128))
                .0
        };
        // Sign-magnitude Q128 sums. A term is slightly negative only when a
        // unit was issued after the paid index passed the drained unit value;
        // a net negative total pays nothing.
        let mut owed = U256::from(self.paid_fraction_q128);
        let mut excess = U256::zero();
        if let Some(stop) = stop {
            // Two's-complement difference of cumulative values; the true
            // magnitude is far below 2^255.
            let per_share = drained_value(&stop)
                .overflowing_sub(drained_value(&self.growth_checkpoint))
                .0;
            if per_share.bit(255) {
                let magnitude = (!per_share).overflowing_add(U256::one()).0;
                excess = excess.saturating_add(magnitude.saturating_mul(U256::from(balance)));
            } else {
                owed = per_share
                    .checked_mul(U256::from(balance))
                    .and_then(|value| value.checked_add(owed))
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
        }
        let held = U256::from(self.units_q64);
        if self.paid_checkpoint_q64 <= DRAINED_UNIT_VALUE_Q64 {
            owed = owed
                .checked_add(held * U256::from(DRAINED_UNIT_VALUE_Q64 - self.paid_checkpoint_q64))
                .ok_or(ErrorCode::MarketMathOverflow)?;
        } else {
            excess = excess.saturating_add(held * U256::from(self.paid_checkpoint_q64 - DRAINED_UNIT_VALUE_Q64));
        }
        let cash_scaled = owed.saturating_sub(excess);
        let whole = cash_scaled >> 128;
        require!(whole <= U256::from(u64::MAX), ErrorCode::MarketMathOverflow);
        self.claimable = self
            .claimable
            .checked_add(whole.as_u64())
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.paid_fraction_q128 = cash_scaled.low_u128();
        self.epoch = book.epoch;
        self.units_q64 = 0;
        self.paid_checkpoint_q64 = 0;
        self.growth_checkpoint = start;
        Ok(())
    }

    pub fn checkpoint(&mut self, balance: u64, book: &IncomeReceivableBook, lane: usize) -> Result<()> {
        self.catch_up_drained(balance, book, lane, IncomeExposureEnd::default())?;
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
