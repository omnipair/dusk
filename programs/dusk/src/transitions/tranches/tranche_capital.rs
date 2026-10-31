use anchor_lang::prelude::*;

use crate::{
    errors::ErrorCode,
    math::{mul_div_ceil_u128, mul_div_u128},
};

/// Junior's portion of the AMM's existing reserve shares. Senior receipts stay
/// one-for-one with the remaining reserve shares, including hLP-held receipts.
/// Junior receipts have their own exchange rate after income, losses and fees.
#[derive(Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
#[cfg_attr(feature = "idl-build", derive(AnchorSerialize, AnchorDeserialize))]
pub struct TrancheCapital {
    pub junior_reserve_shares: u64,
    pub junior_receipt_supply: u64,
    /// Funded, nonredeemable protocol receipts; included in junior supply.
    pub permanent_receipts: u64,
    /// Escrow remains junior until a successful conversion or cancellation.
    pub queued_receipts: u64,
    pub generation: u64,
}

// Fixed Borsh layout, batched to avoid five tiny I/O calls on every swap.
#[cfg(not(feature = "idl-build"))]
impl AnchorSerialize for TrancheCapital {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        let mut bytes = [0; Self::INIT_SPACE];
        for (i, value) in [
            self.junior_reserve_shares,
            self.junior_receipt_supply,
            self.permanent_receipts,
            self.queued_receipts,
            self.generation,
        ]
        .into_iter()
        .enumerate()
        {
            bytes[i * 8..i * 8 + 8].copy_from_slice(&value.to_le_bytes());
        }
        writer.write_all(&bytes)
    }
}

#[cfg(not(feature = "idl-build"))]
impl AnchorDeserialize for TrancheCapital {
    fn deserialize_reader<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let mut bytes = [0; Self::INIT_SPACE];
        reader.read_exact(&mut bytes)?;
        let field = |i: usize| u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().expect("fixed-width field"));
        Ok(Self {
            junior_reserve_shares: field(0),
            junior_receipt_supply: field(1),
            permanent_receipts: field(2),
            queued_receipts: field(3),
            generation: field(4),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrancheLoss {
    pub reserve_shares_burned: u64,
    pub junior_value_before: u128,
    pub senior_loss_value: u128,
    pub junior_wiped: bool,
}

impl TrancheCapital {
    pub fn validate(&self, total_reserve_shares: u64) -> Result<()> {
        require!(
            self.junior_reserve_shares <= total_reserve_shares,
            ErrorCode::BrokenInvariant
        );
        require!(
            self.permanent_receipts <= self.junior_receipt_supply,
            ErrorCode::BrokenInvariant
        );
        require!(
            self.queued_receipts <= self.junior_receipt_supply - self.permanent_receipts,
            ErrorCode::BrokenInvariant
        );
        require!(
            self.junior_reserve_shares == 0 || self.junior_receipt_supply > 0,
            ErrorCode::BrokenInvariant
        );
        Ok(())
    }

    pub fn senior_reserve_shares(&self, total_reserve_shares: u64) -> Result<u64> {
        total_reserve_shares
            .checked_sub(self.junior_reserve_shares)
            .ok_or_else(|| ErrorCode::BrokenInvariant.into())
    }

    pub fn junior_value(&self, total_funded_value: u128, total_reserve_shares: u64) -> Result<u128> {
        self.validate(total_reserve_shares)?;
        if total_reserve_shares == 0 {
            return Ok(0);
        }
        mul_div_u128(
            total_funded_value,
            self.junior_reserve_shares as u128,
            total_reserve_shares as u128,
        )
    }

    /// Shift an existing senior claim to junior without moving either asset.
    /// The instruction must burn exactly `reserve_shares` senior tokens and
    /// mint the returned number of current-generation junior tokens.
    pub fn enter(&mut self, reserve_shares: u64, total_reserve_shares: u64) -> Result<u64> {
        self.validate(total_reserve_shares)?;
        require!(reserve_shares > 0, ErrorCode::AmountZero);
        require_gte!(
            self.senior_reserve_shares(total_reserve_shares)?,
            reserve_shares,
            ErrorCode::InsufficientBalance
        );
        let receipts = if self.junior_receipt_supply == 0 {
            reserve_shares
        } else {
            // A wiped generation must be archived before recapitalization.
            require!(self.junior_reserve_shares > 0, ErrorCode::InvalidArgument);
            u64::try_from(mul_div_u128(
                reserve_shares as u128,
                self.junior_receipt_supply as u128,
                self.junior_reserve_shares as u128,
            )?)
            .map_err(|_| ErrorCode::MarketMathOverflow)?
        };
        require!(receipts > 0, ErrorCode::AmountZero);
        let backing = self
            .junior_reserve_shares
            .checked_add(reserve_shares)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let supply = self
            .junior_receipt_supply
            .checked_add(receipts)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.junior_reserve_shares = backing;
        self.junior_receipt_supply = supply;
        Ok(receipts)
    }

    /// Bootstrap from the creator's already funded senior allocation. The
    /// returned reserve shares are withheld from the creator's senior mint.
    pub fn seed_from_creator(&mut self, creator_reserve_shares: u64, total_reserve_shares: u64) -> Result<u64> {
        require!(
            self.junior_receipt_supply == 0 && self.generation == 0,
            ErrorCode::InvalidArgument
        );
        require!(creator_reserve_shares > 0, ErrorCode::AmountZero);
        let seed = creator_reserve_shares / 1_000 + u64::from(!creator_reserve_shares.is_multiple_of(1_000));
        let receipts = self.enter(seed, total_reserve_shares)?;
        self.permanent_receipts = receipts;
        Ok(seed)
    }

    /// Move already-issued junior receipts to the permanent owner. Their SPL
    /// tokens are burned by the instruction; the funded virtual receipts stay
    /// in supply, so neither backing nor the other holders' prices changes.
    pub fn pay_seed_fee(&mut self, receipts: u64) -> Result<()> {
        let available = self
            .junior_receipt_supply
            .checked_sub(self.permanent_receipts)
            .and_then(|value| value.checked_sub(self.queued_receipts))
            .ok_or(ErrorCode::BrokenInvariant)?;
        require_gte!(available, receipts, ErrorCode::InsufficientBalance);
        self.permanent_receipts = self
            .permanent_receipts
            .checked_add(receipts)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }

    /// Convert a paid, executable exit. Escrow prevents the receipt from also
    /// being transferred or used by another request. Cash never changes here.
    pub fn convert_queued(&mut self, receipts: u64) -> Result<u64> {
        require!(receipts > 0, ErrorCode::AmountZero);
        require_gte!(self.queued_receipts, receipts, ErrorCode::InsufficientBalance);
        require!(
            self.permanent_receipts <= self.junior_receipt_supply
                && self.queued_receipts <= self.junior_receipt_supply - self.permanent_receipts,
            ErrorCode::BrokenInvariant
        );
        let reserve_shares = u64::try_from(mul_div_u128(
            receipts as u128,
            self.junior_reserve_shares as u128,
            self.junior_receipt_supply as u128,
        )?)
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        // Worthless receipts can still be burned, preserving the holder's
        // separately checkpointed income rights in the retired generation.
        self.queued_receipts -= receipts;
        self.junior_receipt_supply -= receipts;
        self.junior_reserve_shares -= reserve_shares;
        Ok(reserve_shares)
    }

    /// Apply only the funded loss left after insurance. Values use one shared
    /// market numeraire; applying a base loss consumes the same junior budget
    /// subsequently available to quote losses. Unpaid interest is excluded.
    pub fn absorb_loss(
        &mut self,
        funded_value_before: u128,
        loss_value: u128,
        total_reserve_shares: &mut u64,
    ) -> Result<TrancheLoss> {
        self.validate(*total_reserve_shares)?;
        require!(loss_value <= funded_value_before, ErrorCode::InvalidArgument);
        let junior_value_before = self.junior_value(funded_value_before, *total_reserve_shares)?;
        let burn = if loss_value == 0 || funded_value_before == 0 {
            0
        } else {
            let proportional = mul_div_ceil_u128(loss_value, *total_reserve_shares as u128, funded_value_before)?;
            self.junior_reserve_shares
                .min(u64::try_from(proportional).map_err(|_| ErrorCode::MarketMathOverflow)?)
        };
        self.junior_reserve_shares -= burn;
        *total_reserve_shares -= burn;
        Ok(TrancheLoss {
            reserve_shares_burned: burn,
            junior_value_before,
            senior_loss_value: loss_value.saturating_sub(junior_value_before),
            junior_wiped: self.junior_reserve_shares == 0 && self.junior_receipt_supply > 0,
        })
    }

    /// Archive income indexes and bind the next mint before calling this.
    /// Outstanding old receipts retain no claim on new funded capital.
    pub fn retire_wiped_generation(&mut self) -> Result<Self> {
        require!(
            self.junior_reserve_shares == 0 && self.junior_receipt_supply > 0,
            ErrorCode::InvalidArgument
        );
        let next = self.generation.checked_add(1).ok_or(ErrorCode::MarketMathOverflow)?;
        let retired = *self;
        *self = Self {
            generation: next,
            ..Self::default()
        };
        Ok(retired)
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/transitions/tranches/tranche_capital.rs");
}
