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
    /// Outstanding units with 64 fractional bits. Fractional issuance keeps a
    /// unit's paid-plus-unpaid value at its face value through an epoch, so a
    /// drained epoch pays every unit exactly that value.
    pub units_q64: u128,
    /// Cash per whole unit (Q64), rounded down at each credit.
    pub paid_per_unit_q64: u128,
    /// Cash (Q64 atoms) that the last credit's rounding left undistributed.
    /// It joins the next credit, so rounding never accumulates: a loss-free
    /// drained epoch's paid index ends within a few 2^-64 of face value.
    /// Packed into the book's alignment padding, so it adds no heap.
    pub paid_remainder_q64: u64,
    /// Ordinary senior, junior, base hLP, quote hLP. Growth is cumulative
    /// across the epochs of one drain run and resets only at an archived roll.
    pub growth: [ReceivableGrowth; 4],
    /// Epochs of the current drain run before this one. Every epoch from
    /// `epoch - run_length` up to the current one ended fully repaid without a
    /// writeoff and was advanced in place, so holders catch up across them
    /// without archived history. Packed with `impaired` to keep the decoded
    /// books inside liquidation heap headroom.
    pub run_length: u32,
    /// Ordinary senior and junior lane units when the current epoch started;
    /// equal to `growth` units at the last in-place advance. Catch-up values
    /// units issued before this point at the drained unit value and units
    /// issued after it at the live paid index, so the starting weighted index
    /// is never needed. The hLP lanes have one market-held claim each,
    /// checkpointed after every issuance, so that claim's own checkpoint is
    /// its lane's start. Units alone, for two lanes, keep the decoded income
    /// books inside liquidation heap headroom.
    pub epoch_start_units_q64: [u128; 2],
    /// A writeoff in this epoch. Such an epoch cannot be advanced in place;
    /// it needs an archived roll.
    pub impaired: bool,
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
