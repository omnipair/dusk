use super::{TrancheRevenueCarry, TrancheRevenueSource};
use crate::{errors::ErrorCode, math::distribute_growth_q64, state::TrancheCapital};
use anchor_lang::prelude::*;

/// Custody-backed junior income. Cached ownership is synchronized by every
/// capital transition; the market invariant checks it against the shared book.
#[derive(Clone, Copy, Debug, Default, InitSpace)]
#[cfg_attr(feature = "idl-build", derive(AnchorSerialize, AnchorDeserialize))]
pub struct TrancheIncome {
    pub initialized: bool,
    pub reserve_shares: u64,
    pub receipt_supply: u64,
    pub swap_growth_q64: u128,
    pub interest_growth_q64: u128,
    pub swap_remainder: u64,
    pub interest_remainder: u64,
    pub seed_swap_amount: u64,
    pub seed_interest_amount: u64,
    pub seed_swap_checkpoint: u128,
    pub seed_interest_checkpoint: u128,
    pub seed_swap_remainder: u64,
    pub seed_interest_remainder: u64,
    pub carry: TrancheRevenueCarry,
}

impl TrancheIncome {
    pub fn checkpoint_seed(&mut self, capital: &TrancheCapital) -> Result<()> {
        let (swap, sr) = crate::math::accrue_fee_liability_with_remainder(
            capital.permanent_receipts,
            self.swap_growth_q64,
            self.seed_swap_checkpoint,
            self.seed_swap_remainder,
        )?;
        let (interest, ir) = crate::math::accrue_fee_liability_with_remainder(
            capital.permanent_receipts,
            self.interest_growth_q64,
            self.seed_interest_checkpoint,
            self.seed_interest_remainder,
        )?;
        self.seed_swap_amount = self
            .seed_swap_amount
            .checked_add(swap)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.seed_interest_amount = self
            .seed_interest_amount
            .checked_add(interest)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.seed_swap_remainder = sr;
        self.seed_interest_remainder = ir;
        self.seed_swap_checkpoint = self.swap_growth_q64;
        self.seed_interest_checkpoint = self.interest_growth_q64;
        Ok(())
    }

    pub fn synchronize(&mut self, capital: &TrancheCapital) {
        self.initialized = true;
        self.reserve_shares = capital.junior_reserve_shares;
        self.receipt_supply = capital.junior_receipt_supply;
    }

    /// Return senior's allocation. Both allocations remain part of the same
    /// custody liability, but use disjoint per-receipt growth indexes.
    pub fn allocate(&mut self, credit: u64, source: TrancheRevenueSource) -> Result<u64> {
        // Before the first funded deposit there are no tranche claims.
        if !self.initialized {
            return Ok(credit);
        }
        let capital = TrancheCapital {
            junior_reserve_shares: self.reserve_shares,
            junior_receipt_supply: self.receipt_supply,
            ..Default::default()
        };
        let allocation = self.carry.allocate(credit, source, &capital)?;
        let swap = source == TrancheRevenueSource::SwapFee;
        let (index, remainder, seed) = if swap {
            (
                &mut self.swap_growth_q64,
                &mut self.swap_remainder,
                &mut self.seed_swap_amount,
            )
        } else {
            (
                &mut self.interest_growth_q64,
                &mut self.interest_remainder,
                &mut self.seed_interest_amount,
            )
        };
        *seed = seed
            .checked_add(allocation.rebuild_seed)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        if allocation.junior > 0 {
            let (growth, carry) = distribute_growth_q64(allocation.junior, self.receipt_supply, *remainder)?;
            *index = index.checked_add(growth).ok_or(ErrorCode::MarketMathOverflow)?;
            *remainder = carry;
        }
        Ok(allocation.senior)
    }

    pub fn senior_supply(&self, common_supply: u64) -> Result<u64> {
        common_supply
            .checked_sub(self.reserve_shares)
            .ok_or_else(|| ErrorCode::BrokenInvariant.into())
    }
}

// The wire layout is ordinary fixed-width Borsh. Batch its reads/writes to
// avoid dozens of tiny writer calls on every swap; IDL generation derives the
// identical schema from the fields above.
#[cfg(not(feature = "idl-build"))]
impl AnchorSerialize for TrancheIncome {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        let mut bytes = [0_u8; Self::INIT_SPACE];
        bytes[0] = u8::from(self.initialized);
        bytes[1..9].copy_from_slice(&self.reserve_shares.to_le_bytes());
        bytes[9..17].copy_from_slice(&self.receipt_supply.to_le_bytes());
        bytes[17..33].copy_from_slice(&self.swap_growth_q64.to_le_bytes());
        bytes[33..49].copy_from_slice(&self.interest_growth_q64.to_le_bytes());
        bytes[49..57].copy_from_slice(&self.swap_remainder.to_le_bytes());
        bytes[57..65].copy_from_slice(&self.interest_remainder.to_le_bytes());
        bytes[65..73].copy_from_slice(&self.seed_swap_amount.to_le_bytes());
        bytes[73..81].copy_from_slice(&self.seed_interest_amount.to_le_bytes());
        bytes[81..97].copy_from_slice(&self.seed_swap_checkpoint.to_le_bytes());
        bytes[97..113].copy_from_slice(&self.seed_interest_checkpoint.to_le_bytes());
        bytes[113..121].copy_from_slice(&self.seed_swap_remainder.to_le_bytes());
        bytes[121..129].copy_from_slice(&self.seed_interest_remainder.to_le_bytes());
        bytes[129] = u8::from(self.carry.interest_half_atom);
        bytes[130] = u8::from(self.carry.swap_half_atom);
        writer.write_all(&bytes)
    }
}

#[cfg(not(feature = "idl-build"))]
impl AnchorDeserialize for TrancheIncome {
    fn deserialize_reader<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let mut bytes = [0_u8; Self::INIT_SPACE];
        reader.read_exact(&mut bytes)?;
        if bytes[0] > 1 || bytes[129] > 1 || bytes[130] > 1 {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        Ok(Self {
            initialized: bytes[0] != 0,
            reserve_shares: u64::from_le_bytes(*arrayref::array_ref![bytes, 1, 8]),
            receipt_supply: u64::from_le_bytes(*arrayref::array_ref![bytes, 9, 8]),
            swap_growth_q64: u128::from_le_bytes(*arrayref::array_ref![bytes, 17, 16]),
            interest_growth_q64: u128::from_le_bytes(*arrayref::array_ref![bytes, 33, 16]),
            swap_remainder: u64::from_le_bytes(*arrayref::array_ref![bytes, 49, 8]),
            interest_remainder: u64::from_le_bytes(*arrayref::array_ref![bytes, 57, 8]),
            seed_swap_amount: u64::from_le_bytes(*arrayref::array_ref![bytes, 65, 8]),
            seed_interest_amount: u64::from_le_bytes(*arrayref::array_ref![bytes, 73, 8]),
            seed_swap_checkpoint: u128::from_le_bytes(*arrayref::array_ref![bytes, 81, 16]),
            seed_interest_checkpoint: u128::from_le_bytes(*arrayref::array_ref![bytes, 97, 16]),
            seed_swap_remainder: u64::from_le_bytes(*arrayref::array_ref![bytes, 113, 8]),
            seed_interest_remainder: u64::from_le_bytes(*arrayref::array_ref![bytes, 121, 8]),
            carry: TrancheRevenueCarry {
                interest_half_atom: bytes[129] != 0,
                swap_half_atom: bytes[130] != 0,
            },
        })
    }
}

#[cfg(test)]
mod encoding_tests {
    use super::*;

    #[test]
    fn tranche_income_encoding_matches_borsh_field_order() {
        let mut bytes = vec![0_u8; TrancheIncome::INIT_SPACE];
        bytes[0] = 1;
        for (index, byte) in bytes[1..129].iter_mut().enumerate() {
            *byte = index as u8;
        }
        bytes[129] = 1;
        let parsed = TrancheIncome::try_from_slice(&bytes).unwrap();
        assert!(parsed.initialized && parsed.carry.interest_half_atom && !parsed.carry.swap_half_atom);
        assert_eq!(parsed.reserve_shares, u64::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7]));
        assert_eq!(
            parsed.seed_interest_remainder,
            u64::from_le_bytes([120, 121, 122, 123, 124, 125, 126, 127])
        );
        assert_eq!(parsed.try_to_vec().unwrap(), bytes);
        for i in [0, 129, 130] {
            let mut invalid = bytes.clone();
            invalid[i] = 2;
            assert!(TrancheIncome::try_from_slice(&invalid).is_err());
        }
    }
}
