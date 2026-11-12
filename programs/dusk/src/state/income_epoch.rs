use anchor_lang::prelude::*;

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

/// Immutable closing state of one unpaid-income denomination. Holders use it
/// to carry their existing units and unpaid cash fractions into the next epoch.
#[account]
#[derive(InitSpace, Default)]
pub struct IncomeEpoch {
    pub market: Pubkey,
    pub book_index: u8,
    pub book: IncomeReceivableBook,
    pub bump: u8,
}
