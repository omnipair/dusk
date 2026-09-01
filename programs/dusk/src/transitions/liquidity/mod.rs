mod hlp;

pub use hlp::*;

use crate::{
    constants::{MIN_LIQUIDITY, NAD},
    errors::ErrorCode,
    math::{ceil_div, normalize_to_nad, SqrtU128},
    state::{Market, MarketAsset},
};
use anchor_lang::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SwapCashFloors {
    base: u64,
    quote: u64,
}

impl SwapCashFloors {
    pub(crate) fn set(&mut self, asset: MarketAsset, amount: u64) {
        match asset {
            MarketAsset::Base => self.base = amount,
            MarketAsset::Quote => self.quote = amount,
        }
    }

    pub(crate) fn available(self, market: &Market) -> bool {
        market.base_side.reserves.cash_reserve >= self.base && market.quote_side.reserves.cash_reserve >= self.quote
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SwapCashPolicy {
    Spot,
    Borrow {
        asset: MarketAsset,
        amount: u64,
    },
    Decrease {
        debt_asset: MarketAsset,
        debt_shares: u128,
        debt_principal: u128,
    },
    Close {
        debt_asset: MarketAsset,
        debt_shares: u128,
        debt_principal: u128,
    },
    Liquidate {
        debt_asset: MarketAsset,
        debt_shares: u128,
        debt_principal: u128,
    },
}

pub(crate) fn reserve_for_ylp_mint_ceil(reserve_before: u64, ylp_supply_before: u64, ylp_amount: u64) -> Result<u64> {
    require!(ylp_supply_before > 0, ErrorCode::InsufficientLiquidity);
    let reserve_amount = ceil_div(
        (ylp_amount as u128)
            .checked_mul(reserve_before as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        ylp_supply_before as u128,
    )
    .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(reserve_amount).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

impl Market {
    pub fn add_liquidity(
        &mut self,
        max_base_reserve_credit: u64,
        max_quote_reserve_credit: u64,
    ) -> Result<AddLiquidityReceipt> {
        let receipt = self.preview_add_liquidity(max_base_reserve_credit, max_quote_reserve_credit)?;
        let supply_before = self.base_side.shares.ylp_supply;
        let internal_mint_amount = receipt
            .ylp_supply
            .checked_sub(supply_before)
            .ok_or(ErrorCode::SupplyUnderflow)?;
        let seeded_price_nad = if supply_before == 0 {
            let base_nad = normalize_to_nad(u128::from(receipt.base_reserve_credit), self.base_side.asset_decimals)?;
            let quote_nad = normalize_to_nad(u128::from(receipt.quote_reserve_credit), self.quote_side.asset_decimals)?;
            let price = quote_nad
                .checked_mul(u128::from(NAD))
                .and_then(|value| value.checked_div(base_nad))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let price = u64::try_from(price).map_err(|_| ErrorCode::MarketMathOverflow)?;
            require!(price > 0, ErrorCode::InvalidSettlementPrice);
            if self.amm.launch_reference_price_nad > 0 {
                require_eq!(
                    price,
                    self.amm.launch_reference_price_nad,
                    ErrorCode::InvalidSettlementPrice
                );
            }
            Some(price)
        } else {
            None
        };

        self.base_side.credit_reserve(receipt.base_reserve_credit, true)?;
        self.quote_side.credit_reserve(receipt.quote_reserve_credit, true)?;
        self.base_side.shares.mint(internal_mint_amount)?;
        self.quote_side.shares.mint(internal_mint_amount)?;
        self.base_side.assert_share_backing()?;
        self.quote_side.assert_share_backing()?;

        if let Some(seeded_price_nad) = seeded_price_nad {
            if self.amm.launch_reference_price_nad == 0 {
                self.amm.launch_reference_price_nad = seeded_price_nad;
            }
            self.initial_liquidity_authority = Pubkey::default();
        }

        Ok(receipt)
    }

    pub fn preview_add_liquidity(
        &self,
        max_base_reserve_credit: u64,
        max_quote_reserve_credit: u64,
    ) -> Result<AddLiquidityReceipt> {
        require!(
            max_base_reserve_credit > 0 && max_quote_reserve_credit > 0,
            ErrorCode::AmountZero
        );
        let base_reserve_before = self.base_side.reserves.live_reserve;
        let quote_reserve_before = self.quote_side.reserves.live_reserve;
        if base_reserve_before > 0 || quote_reserve_before > 0 {
            require!(
                base_reserve_before > 0 && quote_reserve_before > 0,
                ErrorCode::InsufficientLiquidity
            );
        }

        let ylp_amount = self.ylp_for_deposit(
            base_reserve_before,
            quote_reserve_before,
            max_base_reserve_credit,
            max_quote_reserve_credit,
        )?;
        require!(ylp_amount > 0, ErrorCode::SlippageExceeded);

        let (base_reserve_credit, quote_reserve_credit) = if self.base_side.shares.ylp_supply == 0 {
            (max_base_reserve_credit, max_quote_reserve_credit)
        } else {
            let supply_before = self.base_side.shares.ylp_supply;
            let base_reserve_credit = reserve_for_ylp_mint_ceil(base_reserve_before, supply_before, ylp_amount)?;
            let quote_reserve_credit = reserve_for_ylp_mint_ceil(quote_reserve_before, supply_before, ylp_amount)?;
            require_gte!(
                max_base_reserve_credit,
                base_reserve_credit,
                ErrorCode::SlippageExceeded
            );
            require_gte!(
                max_quote_reserve_credit,
                quote_reserve_credit,
                ErrorCode::SlippageExceeded
            );
            (base_reserve_credit, quote_reserve_credit)
        };
        require!(
            base_reserve_credit > 0 && quote_reserve_credit > 0,
            ErrorCode::AmountZero
        );

        let internal_mint_amount = if self.base_side.shares.ylp_supply == 0 {
            ylp_amount.checked_add(MIN_LIQUIDITY).ok_or(ErrorCode::SupplyOverflow)?
        } else {
            ylp_amount
        };
        let ylp_supply = self
            .base_side
            .shares
            .ylp_supply
            .checked_add(internal_mint_amount)
            .ok_or(ErrorCode::SupplyOverflow)?;

        Ok(AddLiquidityReceipt {
            base_reserve_credit,
            quote_reserve_credit,
            ylp_amount,
            ylp_supply,
        })
    }

    pub fn remove_liquidity(&mut self, ylp_amount: u64) -> Result<RemoveLiquidityReceipt> {
        require!(ylp_amount > 0, ErrorCode::AmountZero);
        require_eq!(
            self.base_side.shares.ylp_supply,
            self.quote_side.shares.ylp_supply,
            ErrorCode::BrokenInvariant
        );

        let base_amount_out = self
            .base_side
            .shares
            .reserve_for_burn(self.base_side.reserves.live_reserve, ylp_amount)?;
        let quote_amount_out = self
            .quote_side
            .shares
            .reserve_for_burn(self.quote_side.reserves.live_reserve, ylp_amount)?;
        require_gte!(
            self.base_side.reserves.cash_reserve,
            base_amount_out,
            ErrorCode::InsufficientLiquidity
        );
        require_gte!(
            self.quote_side.reserves.cash_reserve,
            quote_amount_out,
            ErrorCode::InsufficientLiquidity
        );

        self.base_side.debit_reserve(base_amount_out, true)?;
        self.quote_side.debit_reserve(quote_amount_out, true)?;
        self.base_side.shares.burn(ylp_amount)?;
        self.quote_side.shares.burn(ylp_amount)?;
        self.base_side.assert_share_backing()?;
        self.quote_side.assert_share_backing()?;

        // The instruction checkpoints the departing holder before this burn.
        // Once only the permanently burned minimum remains outside the two
        // hLP vaults, discard source-specific funding dust so a later yLP
        // cohort cannot inherit a fraction left by the prior cohort.
        let hlp_ylp_shares = self
            .base_hlp_vault
            .ylp_shares
            .checked_add(self.quote_hlp_vault.ylp_shares)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let non_hlp_ylp_supply = self
            .base_side
            .shares
            .ylp_supply
            .checked_sub(hlp_ylp_shares)
            .ok_or(ErrorCode::BrokenInvariant)?;
        if non_hlp_ylp_supply == MIN_LIQUIDITY {
            self.base_side.fees.hlp_funding_interest_growth_remainder_scaled = 0;
            self.quote_side.fees.hlp_funding_interest_growth_remainder_scaled = 0;
        }

        Ok(RemoveLiquidityReceipt {
            ylp_amount,
            base_amount_out,
            quote_amount_out,
            ylp_supply: self.base_side.shares.ylp_supply,
        })
    }

    pub(crate) fn ylp_for_deposit(
        &self,
        base_reserve_before: u64,
        quote_reserve_before: u64,
        base_amount: u64,
        quote_amount: u64,
    ) -> Result<u64> {
        require_eq!(
            self.base_side.shares.ylp_supply,
            self.quote_side.shares.ylp_supply,
            ErrorCode::BrokenInvariant
        );
        if self.base_side.shares.ylp_supply == 0 {
            // sqrt(amount0_in * amount1_in) - MINIMUM_LIQUIDITY
            // MINIMUM_LIQUIDITY = 1000
            // 9 decimals: 1000 / 10^9 = 1e-6 full LP tokens
            // 1000 units are burned permanently.
            // This burn (~1e-6 of supply) is larger than Uniswap V2's 1e-15 burn (with 18 decimals),
            // but still negligible for users and significantly raises the cost of share inflation attacks.
            return (base_amount as u128)
                .checked_mul(quote_amount as u128)
                .ok_or(ErrorCode::LiquidityMathOverflow)?
                .sqrt()
                .ok_or(ErrorCode::LiquiditySqrtOverflow)?
                .checked_sub(MIN_LIQUIDITY as u128)
                .ok_or(ErrorCode::LiquidityUnderflow)?
                .try_into()
                .map_err(|_| ErrorCode::LiquidityConversionOverflow.into());
        }
        let base_ylp = self
            .base_side
            .shares
            .shares_for_deposit(base_reserve_before, base_amount)?;
        let quote_ylp = self
            .quote_side
            .shares
            .shares_for_deposit(quote_reserve_before, quote_amount)?;
        Ok(base_ylp.min(quote_ylp))
    }
}

pub struct AddLiquidityReceipt {
    pub base_reserve_credit: u64,
    pub quote_reserve_credit: u64,
    pub ylp_amount: u64,
    pub ylp_supply: u64,
}

pub struct RemoveLiquidityReceipt {
    pub ylp_amount: u64,
    pub base_amount_out: u64,
    pub quote_amount_out: u64,
    pub ylp_supply: u64,
}
