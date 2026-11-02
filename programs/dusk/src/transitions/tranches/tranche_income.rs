use super::{TrancheRevenueCarry, TrancheRevenueSource};
use crate::{errors::ErrorCode, math::distribute_growth_q64, state::TrancheCapital};
use anchor_lang::prelude::*;

/// Custody-backed junior income. Cached ownership is synchronized by every
/// capital transition; the market invariant checks it against the shared book.
#[derive(Clone, Copy, Debug, InitSpace)]
#[cfg_attr(feature = "idl-build", derive(AnchorSerialize, AnchorDeserialize))]
pub struct TrancheIncome {
    pub initialized: bool,
    pub income_share_bps: u16,
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

impl Default for TrancheIncome {
    fn default() -> Self {
        Self {
            initialized: false,
            income_share_bps: crate::constants::DEFAULT_JUNIOR_INCOME_SHARE_BPS,
            reserve_shares: 0,
            receipt_supply: 0,
            swap_growth_q64: 0,
            interest_growth_q64: 0,
            swap_remainder: 0,
            interest_remainder: 0,
            seed_swap_amount: 0,
            seed_interest_amount: 0,
            seed_swap_checkpoint: 0,
            seed_interest_checkpoint: 0,
            seed_swap_remainder: 0,
            seed_interest_remainder: 0,
            carry: TrancheRevenueCarry::default(),
        }
    }
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
        let allocation = self.carry.allocate(credit, source, &capital, self.income_share_bps)?;
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
        bytes[1..3].copy_from_slice(&self.income_share_bps.to_le_bytes());
        bytes[3..11].copy_from_slice(&self.reserve_shares.to_le_bytes());
        bytes[11..19].copy_from_slice(&self.receipt_supply.to_le_bytes());
        bytes[19..35].copy_from_slice(&self.swap_growth_q64.to_le_bytes());
        bytes[35..51].copy_from_slice(&self.interest_growth_q64.to_le_bytes());
        bytes[51..59].copy_from_slice(&self.swap_remainder.to_le_bytes());
        bytes[59..67].copy_from_slice(&self.interest_remainder.to_le_bytes());
        bytes[67..75].copy_from_slice(&self.seed_swap_amount.to_le_bytes());
        bytes[75..83].copy_from_slice(&self.seed_interest_amount.to_le_bytes());
        bytes[83..99].copy_from_slice(&self.seed_swap_checkpoint.to_le_bytes());
        bytes[99..115].copy_from_slice(&self.seed_interest_checkpoint.to_le_bytes());
        bytes[115..123].copy_from_slice(&self.seed_swap_remainder.to_le_bytes());
        bytes[123..131].copy_from_slice(&self.seed_interest_remainder.to_le_bytes());
        bytes[131..133].copy_from_slice(&self.carry.interest_remainder_bps.to_le_bytes());
        bytes[133..135].copy_from_slice(&self.carry.swap_remainder_bps.to_le_bytes());
        writer.write_all(&bytes)
    }
}

#[cfg(not(feature = "idl-build"))]
impl AnchorDeserialize for TrancheIncome {
    fn deserialize_reader<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let mut bytes = [0_u8; Self::INIT_SPACE];
        reader.read_exact(&mut bytes)?;
        if bytes[0] > 1 {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        Ok(Self {
            initialized: bytes[0] != 0,
            income_share_bps: u16::from_le_bytes(*arrayref::array_ref![bytes, 1, 2]),
            reserve_shares: u64::from_le_bytes(*arrayref::array_ref![bytes, 3, 8]),
            receipt_supply: u64::from_le_bytes(*arrayref::array_ref![bytes, 11, 8]),
            swap_growth_q64: u128::from_le_bytes(*arrayref::array_ref![bytes, 19, 16]),
            interest_growth_q64: u128::from_le_bytes(*arrayref::array_ref![bytes, 35, 16]),
            swap_remainder: u64::from_le_bytes(*arrayref::array_ref![bytes, 51, 8]),
            interest_remainder: u64::from_le_bytes(*arrayref::array_ref![bytes, 59, 8]),
            seed_swap_amount: u64::from_le_bytes(*arrayref::array_ref![bytes, 67, 8]),
            seed_interest_amount: u64::from_le_bytes(*arrayref::array_ref![bytes, 75, 8]),
            seed_swap_checkpoint: u128::from_le_bytes(*arrayref::array_ref![bytes, 83, 16]),
            seed_interest_checkpoint: u128::from_le_bytes(*arrayref::array_ref![bytes, 99, 16]),
            seed_swap_remainder: u64::from_le_bytes(*arrayref::array_ref![bytes, 115, 8]),
            seed_interest_remainder: u64::from_le_bytes(*arrayref::array_ref![bytes, 123, 8]),
            carry: TrancheRevenueCarry {
                interest_remainder_bps: u16::from_le_bytes(*arrayref::array_ref![bytes, 131, 2]),
                swap_remainder_bps: u16::from_le_bytes(*arrayref::array_ref![bytes, 133, 2]),
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
        for (index, byte) in bytes[1..131].iter_mut().enumerate() {
            *byte = index as u8;
        }
        bytes[131..133].copy_from_slice(&9_999_u16.to_le_bytes());
        bytes[133..135].copy_from_slice(&5_000_u16.to_le_bytes());
        let parsed = TrancheIncome::try_from_slice(&bytes).unwrap();
        assert!(parsed.initialized);
        assert_eq!(parsed.carry.interest_remainder_bps, 9_999);
        assert_eq!(parsed.carry.swap_remainder_bps, 5_000);
        assert_eq!(parsed.reserve_shares, u64::from_le_bytes([2, 3, 4, 5, 6, 7, 8, 9]));
        assert_eq!(
            parsed.seed_interest_remainder,
            u64::from_le_bytes([122, 123, 124, 125, 126, 127, 128, 129])
        );
        assert_eq!(parsed.try_to_vec().unwrap(), bytes);
        for i in [0] {
            let mut invalid = bytes.clone();
            invalid[i] = 2;
            assert!(TrancheIncome::try_from_slice(&invalid).is_err());
        }
    }
}
