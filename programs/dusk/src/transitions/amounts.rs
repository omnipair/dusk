use anchor_lang::prelude::*;

use crate::{constants::NAD_DECIMALS, errors::ErrorCode, math::rescale_amount, state::Market};

impl Market {
    // Both assets use one immutable quantity scale. Raising it to the more
    // precise mint preserves every token atom; ratios and prices remain NAD.
    pub(crate) fn amount_decimals(&self) -> u8 {
        NAD_DECIMALS
            .max(self.base_side.asset_decimals)
            .max(self.quote_side.asset_decimals)
    }

    pub(crate) fn normalize_amount(&self, amount: u128, asset_decimals: u8) -> Result<u128> {
        rescale_amount(amount, asset_decimals, self.amount_decimals(), false)
    }

    pub(crate) fn denormalize_amount_floor(&self, amount: u128, asset_decimals: u8) -> Result<u64> {
        let raw = rescale_amount(amount, self.amount_decimals(), asset_decimals, false)?;
        u64::try_from(raw).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub(crate) fn denormalize_amount_ceil(&self, amount: u128, asset_decimals: u8) -> Result<u64> {
        let raw = rescale_amount(amount, self.amount_decimals(), asset_decimals, true)?;
        u64::try_from(raw).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("../tests/transitions/amounts.rs");
}
