//! Publication of interest credited to an hLP vault at accrual.
use super::*;

impl HlpVault {
    /// Credit interest to this vault's holder index, or hold it as
    /// unallocated while the vault has no hLP supply.
    pub(crate) fn publish_new_interest(&mut self, asset: MarketAsset, amount: u64) -> Result<()> {
        let (unallocated, index, carry) = match asset {
            MarketAsset::Base => (
                &mut self.unallocated_base_interest_amount,
                &mut self.base_interest_growth_index_q64,
                &mut self.base_interest_growth_remainder_scaled,
            ),
            MarketAsset::Quote => (
                &mut self.unallocated_quote_interest_amount,
                &mut self.quote_interest_growth_index_q64,
                &mut self.quote_interest_growth_remainder_scaled,
            ),
        };
        credit_hlp_growth(self.hlp_supply, unallocated, index, carry, amount)
    }

    /// Interest held while frozen, as `(target asset, borrowed asset)`.
    pub(crate) fn held_interest(&self, target: MarketAsset) -> (u64, u64) {
        match target {
            MarketAsset::Base => (self.held_base_interest, self.held_quote_interest),
            MarketAsset::Quote => (self.held_quote_interest, self.held_base_interest),
        }
    }

    /// A vault that resumes owes nothing beyond its debt: the interest it
    /// held for its creditors goes to its holders. Both amounts are already
    /// in `credited`, so nothing moves the curve.
    pub(crate) fn release_held_interest(&mut self) -> Result<()> {
        let base = core::mem::take(&mut self.held_base_interest);
        let quote = core::mem::take(&mut self.held_quote_interest);
        self.publish_new_interest(MarketAsset::Base, base)?;
        self.publish_new_interest(MarketAsset::Quote, quote)
    }
}
