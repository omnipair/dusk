use anchor_lang::prelude::*;

use super::{accrue_fee_liability, Debt, DebtClearance, Market, MarketAsset, MarketSide};
use crate::{constants::NAD, errors::ErrorCode};

pub(crate) use crate::state::market::transitions::hedge::HlpRebalanceReceipt;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct HlpVault {
    pub target_side: u8,
    pub ylp_vault: Pubkey,
    pub ylp_shares: u64,
    /// hLP-owned live reserve depth that is not backed by reserve cash or
    /// normal cash-backed debt. This is the explicit synthetic live component
    /// in `r_virtual = r_cash + r_cash_backed_debt + r_hlp_live`.
    pub base_hlp_live_reserve: u64,
    pub quote_hlp_live_reserve: u64,
    /// Funding debt used by the hLP vault. It accrues interest and counts
    /// toward utilization, but is not same-side cash-backed reserve debt.
    pub debt_shares: u128,
    pub debt_principal: u128,
    pub hlp_supply: u64,
    pub pending_rebalance: i128,
    pub base_swap_fee_growth_index_nad: u128,
    pub base_interest_growth_index_nad: u128,
    pub quote_swap_fee_growth_index_nad: u128,
    pub quote_interest_growth_index_nad: u128,
    pub base_swap_fee_checkpoint_nad: u128,
    pub base_interest_checkpoint_nad: u128,
    pub quote_swap_fee_checkpoint_nad: u128,
    pub quote_interest_checkpoint_nad: u128,
    pub unallocated_base_swap_fee_amount: u64,
    pub unallocated_base_interest_amount: u64,
    pub unallocated_quote_swap_fee_amount: u64,
    pub unallocated_quote_interest_amount: u64,
    pub last_nav_nad: u128,
    pub cached_settlement_price_nad: u128,
    pub last_rebalance_slot: u64,
}

impl HlpVault {
    pub fn initialize(&mut self, target_side: MarketAsset, ylp_vault: Pubkey, current_slot: u64) {
        self.target_side = target_side.code();
        self.ylp_vault = ylp_vault;
        self.last_rebalance_slot = current_slot;
    }

    pub fn target_asset(&self) -> Result<MarketAsset> {
        MarketAsset::try_from_code(self.target_side)
    }

    pub fn mint_hlp(&mut self, amount: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::AmountZero);
        self.hlp_supply = self
            .hlp_supply
            .checked_add(amount)
            .ok_or(ErrorCode::SupplyOverflow)?;
        Ok(())
    }

    pub fn burn_hlp(&mut self, amount: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::AmountZero);
        self.hlp_supply = self
            .hlp_supply
            .checked_sub(amount)
            .ok_or(ErrorCode::SupplyUnderflow)?;
        if self.hlp_supply == 0 {
            require_eq!(self.ylp_shares, 0, ErrorCode::BrokenInvariant);
            require_eq!(self.base_hlp_live_reserve, 0, ErrorCode::BrokenInvariant);
            require_eq!(self.quote_hlp_live_reserve, 0, ErrorCode::BrokenInvariant);
            require_eq!(self.debt_shares, 0, ErrorCode::BrokenInvariant);
            require_eq!(self.debt_principal, 0, ErrorCode::BrokenInvariant);
        }
        Ok(())
    }

    pub fn credit_ylp(&mut self, shares: u64) -> Result<()> {
        self.ylp_shares = self
            .ylp_shares
            .checked_add(shares)
            .ok_or(ErrorCode::SupplyOverflow)?;
        Ok(())
    }

    pub fn debit_ylp(&mut self, shares: u64) -> Result<()> {
        self.ylp_shares = self
            .ylp_shares
            .checked_sub(shares)
            .ok_or(ErrorCode::SupplyUnderflow)?;
        Ok(())
    }

    pub fn hlp_live_reserve(&self, asset: MarketAsset) -> u64 {
        match asset {
            MarketAsset::Base => self.base_hlp_live_reserve,
            MarketAsset::Quote => self.quote_hlp_live_reserve,
        }
    }

    pub fn credit_hlp_live_reserve(&mut self, asset: MarketAsset, amount: u64) -> Result<()> {
        let reserve = match asset {
            MarketAsset::Base => &mut self.base_hlp_live_reserve,
            MarketAsset::Quote => &mut self.quote_hlp_live_reserve,
        };
        *reserve = reserve
            .checked_add(amount)
            .ok_or(ErrorCode::ReserveOverflow)?;
        Ok(())
    }

    pub fn debit_hlp_live_reserve(&mut self, asset: MarketAsset, amount: u64) -> Result<()> {
        let reserve = match asset {
            MarketAsset::Base => &mut self.base_hlp_live_reserve,
            MarketAsset::Quote => &mut self.quote_hlp_live_reserve,
        };
        *reserve = reserve
            .checked_sub(amount)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        Ok(())
    }

    pub fn add_debt_shares(&mut self, shares: u128) -> Result<()> {
        self.debt_shares = self
            .debt_shares
            .checked_add(shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        Ok(())
    }

    pub fn add_debt_principal(&mut self, amount: u64) -> Result<()> {
        self.debt_principal = self
            .debt_principal
            .checked_add(amount as u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        Ok(())
    }

    pub fn clear_debt_repay(
        &mut self,
        repaid: u64,
        shares_burned: u128,
        borrow_index_nad: u128,
    ) -> Result<DebtClearance> {
        require!(repaid > 0, ErrorCode::AmountZero);
        require!(shares_burned > 0, ErrorCode::DebtShareDivisionOverflow);
        require_gte!(
            self.debt_shares,
            shares_burned,
            ErrorCode::DebtShareMathOverflow
        );
        let total_debt = Debt::shares_to_debt(self.debt_shares, borrow_index_nad)?;
        require_gte!(total_debt, repaid as u128, ErrorCode::InsufficientDebt);
        let remaining_shares = self
            .debt_shares
            .checked_sub(shares_burned)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        let remaining_debt = Debt::shares_to_debt(remaining_shares, borrow_index_nad)?;
        let debt_reduced_u128 = total_debt
            .checked_sub(remaining_debt)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        require_gte!(
            debt_reduced_u128,
            repaid as u128,
            ErrorCode::DebtMathOverflow
        );
        let debt_reduced =
            u64::try_from(debt_reduced_u128).map_err(|_| ErrorCode::DebtMathOverflow)?;

        let principal = self.debt_principal.min(total_debt);
        let (principal_paid, interest_paid) =
            crate::math::realized_interest_split(repaid, total_debt, principal)?;
        let (principal_reduced, _) =
            crate::math::realized_interest_split(debt_reduced, total_debt, principal)?;
        self.debt_shares = remaining_shares;
        self.debt_principal = self
            .debt_principal
            .saturating_sub(principal_reduced as u128);
        if self.debt_shares == 0 {
            self.debt_principal = 0;
        }

        Ok(DebtClearance {
            shares_burned,
            debt_reduced,
            principal_paid,
            interest_paid,
            remaining_debt: u64::try_from(remaining_debt)
                .map_err(|_| ErrorCode::DebtMathOverflow)?,
        })
    }

    pub fn checkpoint_yield_from_ylp(
        &mut self,
        base_side: &MarketSide,
        quote_side: &MarketSide,
    ) -> Result<()> {
        self.checkpoint_yield_from_ylp_shares(base_side, quote_side, self.ylp_shares)
    }

    pub fn checkpoint_yield_from_ylp_shares(
        &mut self,
        base_side: &MarketSide,
        quote_side: &MarketSide,
        eligible_ylp_shares: u64,
    ) -> Result<()> {
        let base_swap_fee_amount = accrue_fee_liability(
            eligible_ylp_shares,
            base_side.fees.swap_fee_growth_index_nad,
            self.base_swap_fee_checkpoint_nad,
        )?;
        let base_interest_amount = accrue_fee_liability(
            eligible_ylp_shares,
            base_side.fees.interest_growth_index_nad,
            self.base_interest_checkpoint_nad,
        )?;
        let quote_swap_fee_amount = accrue_fee_liability(
            eligible_ylp_shares,
            quote_side.fees.swap_fee_growth_index_nad,
            self.quote_swap_fee_checkpoint_nad,
        )?;
        let quote_interest_amount = accrue_fee_liability(
            eligible_ylp_shares,
            quote_side.fees.interest_growth_index_nad,
            self.quote_interest_checkpoint_nad,
        )?;

        credit_hlp_growth(
            self.hlp_supply,
            &mut self.unallocated_base_swap_fee_amount,
            &mut self.base_swap_fee_growth_index_nad,
            base_swap_fee_amount,
        )?;
        credit_hlp_growth(
            self.hlp_supply,
            &mut self.unallocated_base_interest_amount,
            &mut self.base_interest_growth_index_nad,
            base_interest_amount,
        )?;
        credit_hlp_growth(
            self.hlp_supply,
            &mut self.unallocated_quote_swap_fee_amount,
            &mut self.quote_swap_fee_growth_index_nad,
            quote_swap_fee_amount,
        )?;
        credit_hlp_growth(
            self.hlp_supply,
            &mut self.unallocated_quote_interest_amount,
            &mut self.quote_interest_growth_index_nad,
            quote_interest_amount,
        )?;

        self.base_swap_fee_checkpoint_nad = base_side.fees.swap_fee_growth_index_nad;
        self.base_interest_checkpoint_nad = base_side.fees.interest_growth_index_nad;
        self.quote_swap_fee_checkpoint_nad = quote_side.fees.swap_fee_growth_index_nad;
        self.quote_interest_checkpoint_nad = quote_side.fees.interest_growth_index_nad;
        Ok(())
    }

    pub fn yield_growth_indexes(&self, market_asset: MarketAsset) -> (u128, u128) {
        match market_asset {
            MarketAsset::Base => (
                self.base_swap_fee_growth_index_nad,
                self.base_interest_growth_index_nad,
            ),
            MarketAsset::Quote => (
                self.quote_swap_fee_growth_index_nad,
                self.quote_interest_growth_index_nad,
            ),
        }
    }
}

fn credit_hlp_growth(
    hlp_supply: u64,
    unallocated_amount: &mut u64,
    growth_index_nad: &mut u128,
    new_amount: u64,
) -> Result<()> {
    *unallocated_amount = unallocated_amount
        .checked_add(new_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    if hlp_supply == 0 || *unallocated_amount == 0 {
        return Ok(());
    }
    let growth_delta = (*unallocated_amount as u128)
        .checked_mul(NAD as u128)
        .and_then(|value| value.checked_div(hlp_supply as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    if growth_delta == 0 {
        return Ok(());
    }
    let allocated = growth_delta
        .checked_mul(hlp_supply as u128)
        .and_then(|value| value.checked_div(NAD as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let allocated = u64::try_from(allocated).map_err(|_| ErrorCode::MarketMathOverflow)?;
    if allocated == 0 {
        return Ok(());
    }
    *growth_index_nad = growth_index_nad
        .checked_add(growth_delta)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    *unallocated_amount = unallocated_amount
        .checked_sub(allocated)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(())
}

impl Market {
    pub fn hlp_yield_growth_indexes(&self, market_asset: MarketAsset) -> (u128, u128) {
        match market_asset {
            MarketAsset::Base => self.base_hlp_vault.yield_growth_indexes(MarketAsset::Base),
            MarketAsset::Quote => self
                .quote_hlp_vault
                .yield_growth_indexes(MarketAsset::Quote),
        }
    }

    pub fn deposit_single_sided(
        &mut self,
        target_asset: MarketAsset,
        deposit_amount: u64,
        min_hlp_amount: u64,
    ) -> Result<crate::state::market::transitions::hedge::HedgeReceipt> {
        crate::state::market::transitions::hedge::DepositSingleSided::new(
            target_asset,
            deposit_amount,
            min_hlp_amount,
        )
        .apply(self)
    }

    pub fn withdraw_single_sided(
        &mut self,
        target_asset: MarketAsset,
        hlp_amount: u64,
    ) -> Result<crate::state::market::transitions::hedge::HedgeReceipt> {
        crate::state::market::transitions::hedge::WithdrawSingleSided::new(target_asset, hlp_amount)
            .apply(self)
    }

    pub fn checkpoint_hlp_vaults(&mut self, current_slot: u64) -> Result<(i128, i128)> {
        crate::state::market::transitions::hedge::checkpoint_hlp_vaults(self, current_slot)
    }

    pub fn rebalance_hlp_vaults(
        &mut self,
        current_slot: u64,
    ) -> Result<(
        crate::state::market::transitions::hedge::HlpRebalanceReceipt,
        crate::state::market::transitions::hedge::HlpRebalanceReceipt,
    )> {
        crate::state::market::transitions::hedge::rebalance_hlp_vaults(self, current_slot)
    }

    pub fn rebalance_hlp_vault_for_swap(
        &mut self,
        preferred_asset: MarketAsset,
        current_slot: u64,
    ) -> Result<(
        crate::state::market::transitions::hedge::HlpRebalanceReceipt,
        crate::state::market::transitions::hedge::HlpRebalanceReceipt,
    )> {
        crate::state::market::transitions::hedge::rebalance_hlp_vault_for_swap(
            self,
            preferred_asset,
            current_slot,
        )
    }

    pub fn pre_solve_hlp_vaults_for_swap(
        &mut self,
        asset_in: MarketAsset,
        amount_in_after_fee: u64,
        current_slot: u64,
    ) -> Result<(
        crate::state::market::transitions::hedge::HlpRebalanceReceipt,
        crate::state::market::transitions::hedge::HlpRebalanceReceipt,
    )> {
        crate::state::market::transitions::hedge::pre_solve_hlp_vaults_for_swap(
            self,
            asset_in,
            amount_in_after_fee,
            current_slot,
        )
    }

    pub fn checkpoint_hlp_yield_from_ylp(&mut self, target_asset: MarketAsset) -> Result<()> {
        crate::state::market::transitions::hedge::checkpoint_hlp_yield_from_ylp(self, target_asset)
    }

    pub fn checkpoint_hlp_yield_from_ylp_shares(
        &mut self,
        target_asset: MarketAsset,
        eligible_ylp_shares: u64,
    ) -> Result<()> {
        crate::state::market::transitions::hedge::checkpoint_hlp_yield_from_ylp_shares(
            self,
            target_asset,
            eligible_ylp_shares,
        )
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/state/hlp.rs");
}
