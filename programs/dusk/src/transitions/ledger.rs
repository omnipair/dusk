use anchor_lang::prelude::*;

use crate::transitions::{
    lending::total_cash_backed_borrowed,
    revenue::{split_protocol_auction_fee, split_revenue},
    SwapFeeBreakdown,
};
use crate::{constants::*, errors::ErrorCode, math::*, state::*};

impl ReserveShares {
    pub fn shares_for_deposit(&self, reserve_before: u128, deposit_amount: u64) -> Result<u64> {
        if self.srlp_supply == 0 || reserve_before == 0 {
            return Ok(deposit_amount);
        }
        let shares = (deposit_amount as u128)
            .checked_mul(self.srlp_supply as u128)
            .and_then(|value| value.checked_div(reserve_before))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        u64::try_from(shares).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub fn reserve_for_burn(&self, reserve_before: u64, share_amount: u64) -> Result<u64> {
        require!(share_amount > 0, ErrorCode::AmountZero);
        require_gte!(self.srlp_supply, share_amount, ErrorCode::InsufficientBalance);
        let reserve_amount = (share_amount as u128)
            .checked_mul(reserve_before as u128)
            .and_then(|value| value.checked_div(self.srlp_supply as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        u64::try_from(reserve_amount).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub fn mint(&mut self, share_amount: u64) -> Result<()> {
        require!(share_amount > 0, ErrorCode::AmountZero);
        self.srlp_supply = self
            .srlp_supply
            .checked_add(share_amount)
            .ok_or(ErrorCode::SupplyOverflow)?;
        Ok(())
    }

    pub fn burn(&mut self, share_amount: u64) -> Result<()> {
        require!(share_amount > 0, ErrorCode::AmountZero);
        self.srlp_supply = self
            .srlp_supply
            .checked_sub(share_amount)
            .ok_or(ErrorCode::SupplyUnderflow)?;
        Ok(())
    }
}

impl Reserves {
    pub fn hlp_backing_inventory(&self, target_asset: MarketAsset) -> u64 {
        match target_asset {
            MarketAsset::Base => self.base_hlp_backing_inventory,
            MarketAsset::Quote => self.quote_hlp_backing_inventory,
        }
    }

    #[inline(always)]
    pub fn total_hlp_backing_inventory(&self) -> Result<u64> {
        self.base_hlp_backing_inventory
            .checked_add(self.quote_hlp_backing_inventory)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub fn debit_hlp_backing_inventory(&mut self, target_asset: MarketAsset, amount: u64) -> Result<()> {
        let inventory = match target_asset {
            MarketAsset::Base => &mut self.base_hlp_backing_inventory,
            MarketAsset::Quote => &mut self.quote_hlp_backing_inventory,
        };
        *inventory = inventory.checked_sub(amount).ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }
}

impl FeesReceipt {
    fn from_side(market_side: &MarketSide) -> Result<Self> {
        let fees = &market_side.fees;
        Ok(Self {
            swap_fee_growth_index_q64: fees.swap_fee_growth_index_q64,
            interest_growth_index_q64: fees.interest_growth_index_q64,
            swap_fee_liability: fees.swap_fee_liability,
            interest_liability: fees.interest_liability,
            unallocated_swap_fee_liability: fees.unallocated_swap_fee_liability,
            unallocated_interest_liability: fees.unallocated_interest_liability,
            referral_interest_liability: fees.referral_interest_liability,
            protocol_fee_liability: fees.protocol_fee_liability()?,
            buyback_fee_liability: fees.buyback_fee_liability()?,
            swap_fee_custody_balance: fees.swap_fee_custody_balance,
            interest_vault_balance: fees.interest_vault_balance,
        })
    }
}

impl MarketAsset {
    pub fn code(self) -> u8 {
        match self {
            Self::Base => 0,
            Self::Quote => 1,
        }
    }

    pub fn try_from_code(code: u8) -> Result<Self> {
        match code {
            0 => Ok(Self::Base),
            1 => Ok(Self::Quote),
            _ => err!(ErrorCode::InvalidArgument),
        }
    }

    pub fn opposite(self) -> Self {
        match self {
            Self::Base => Self::Quote,
            Self::Quote => Self::Base,
        }
    }
}

impl MarketSide {
    /// Collected junior underwriting revenue. Existing
    /// protocol and executor allocations have already been deducted.
    pub fn record_junior_liquidation_credit(&mut self, credit: u64) -> Result<()> {
        require!(self.fees.junior.initialized, ErrorCode::BrokenInvariant);
        let senior = self.fees.junior.allocate(
            credit,
            crate::transitions::tranches::TrancheRevenueSource::LiquidationCoverage,
        )?;
        require_eq!(senior, 0, ErrorCode::BrokenInvariant);
        self.fees.interest_vault_balance = self
            .fees
            .interest_vault_balance
            .checked_add(credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_liability = self
            .fees
            .interest_liability
            .checked_add(credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.assert_backed()
    }

    /// Book the already-selected protocol share without another LP deduction.
    pub(crate) fn record_liquidation_protocol_credit(
        &mut self,
        protocol: u64,
        split: ProtocolAuctionSplit,
    ) -> Result<()> {
        let (fee, buyback) = split_protocol_auction_fee(protocol, &split)?;
        self.fees.interest_vault_balance = self
            .fees
            .interest_vault_balance
            .checked_add(protocol)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        self.fees.interest_protocol_fee_liability = self
            .fees
            .interest_protocol_fee_liability
            .checked_add(fee)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        self.fees.interest_buyback_fee_liability = self
            .fees
            .interest_buyback_fee_liability
            .checked_add(buyback)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        self.fees.assert_backed()
    }

    #[inline(always)]
    pub fn assert_share_backing(&self) -> Result<()> {
        if self.shares.srlp_supply == 0 {
            require_eq!(self.reserves.live_reserve, 0, ErrorCode::BrokenInvariant);
        }
        Ok(())
    }

    pub fn srlp_exchange_rate_nad(&self) -> Result<u128> {
        if self.shares.srlp_supply == 0 {
            return Ok(0);
        }
        self.reserves
            .live_reserve
            .checked_mul(crate::constants::NAD as u128)
            .and_then(|value| value.checked_div(self.shares.srlp_supply as u128))
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub fn credit_reserve(&mut self, amount: u64, credit_cash: bool) -> Result<()> {
        self.reserves.live_reserve = self
            .reserves
            .live_reserve
            .checked_add(u128::from(amount))
            .ok_or(ErrorCode::ReserveOverflow)?;
        if credit_cash {
            self.reserves.cash_reserve = self
                .reserves
                .cash_reserve
                .checked_add(amount)
                .ok_or(ErrorCode::ReserveOverflow)?;
        }
        Ok(())
    }

    pub fn debit_reserve(&mut self, amount: u64, debit_cash: bool) -> Result<()> {
        self.reserves.live_reserve = self
            .reserves
            .live_reserve
            .checked_sub(u128::from(amount))
            .ok_or(ErrorCode::ReserveUnderflow)?;
        if debit_cash {
            self.reserves.cash_reserve = self
                .reserves
                .cash_reserve
                .checked_sub(amount)
                .ok_or(ErrorCode::CashReserveUnderflow)?;
        }
        Ok(())
    }

    /// Records the claimable remainder of a swap fee after a separately
    /// materialized principal-compounding credit. Protocol revenue is always
    /// split from the full base fee before any LP-owned atoms compound.
    #[allow(clippy::too_many_arguments)]
    pub fn record_swap_fee_allocation(
        &mut self,
        base_fee_credit: u64,
        distributed_dynamic_surcharge_credit: u64,
        compounded_base_fee_credit: u64,
        compounded_dynamic_surcharge_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        eligible_srlp_supply: u64,
    ) -> Result<FeesReceipt> {
        if base_fee_credit == 0 && distributed_dynamic_surcharge_credit == 0 {
            require_eq!(compounded_base_fee_credit, 0, ErrorCode::BrokenInvariant);
            require_eq!(compounded_dynamic_surcharge_credit, 0, ErrorCode::BrokenInvariant);
            return FeesReceipt::from_side(self);
        }
        let (protocol_fee, base_lp_fee) = split_revenue(base_fee_credit, protocol_fee_bps)?;
        require_gte!(base_lp_fee, compounded_base_fee_credit, ErrorCode::BrokenInvariant);
        require_gte!(
            distributed_dynamic_surcharge_credit,
            compounded_dynamic_surcharge_credit,
            ErrorCode::BrokenInvariant
        );
        let claimable_base_lp_fee = base_lp_fee
            .checked_sub(compounded_base_fee_credit)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        let claimable_dynamic_surcharge = distributed_dynamic_surcharge_credit
            .checked_sub(compounded_dynamic_surcharge_credit)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        let lp_fee = claimable_base_lp_fee
            .checked_add(claimable_dynamic_surcharge)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let claimable_fee_credit = protocol_fee.checked_add(lp_fee).ok_or(ErrorCode::MarketMathOverflow)?;
        let (fee_auction_amount, buyback_auction_amount) =
            split_protocol_auction_fee(protocol_fee, &protocol_auction_split)?;
        self.fees.swap_fee_custody_balance = self
            .fees
            .swap_fee_custody_balance
            .checked_add(claimable_fee_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.swap_protocol_fee_liability = self
            .fees
            .swap_protocol_fee_liability
            .checked_add(fee_auction_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.swap_buyback_fee_liability = self
            .fees
            .swap_buyback_fee_liability
            .checked_add(buyback_auction_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.unallocated_swap_fee_liability = self
            .fees
            .unallocated_swap_fee_liability
            .checked_add(lp_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.carry_forward_swap_fees_with_supply(eligible_srlp_supply)?;
        self.fees.assert_backed()?;
        FeesReceipt::from_side(self)
    }

    /// Record interest paid by an hLP funding position. Its LP share is
    /// indexed over ordinary srLP plus the permanently burned minimum shares,
    /// using a source-specific carry so public-interest rounding cannot cross
    /// into or out of this distribution lane.
    pub fn record_hlp_funding_interest_credit_with_supply(
        &mut self,
        interest_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        eligible_srlp_supply: u64,
    ) -> Result<FeesReceipt> {
        if interest_credit == 0 {
            return FeesReceipt::from_side(self);
        }
        require!(eligible_srlp_supply > 0, ErrorCode::BrokenInvariant);
        let (protocol_fee, lp_interest) = split_revenue(interest_credit, protocol_fee_bps)?;
        let (fee_auction_amount, buyback_auction_amount) =
            split_protocol_auction_fee(protocol_fee, &protocol_auction_split)?;
        let senior = self.fees.junior.allocate(
            lp_interest,
            crate::transitions::tranches::TrancheRevenueSource::BorrowInterest,
        )?;
        let eligible_srlp_supply = self.fees.junior.senior_supply(eligible_srlp_supply)?;
        let (growth_delta, remainder_scaled) = distribute_growth_q64(
            senior,
            eligible_srlp_supply,
            self.fees.hlp_funding_interest_growth_remainder_scaled,
        )?;

        self.fees.interest_vault_balance = self
            .fees
            .interest_vault_balance
            .checked_add(interest_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_protocol_fee_liability = self
            .fees
            .interest_protocol_fee_liability
            .checked_add(fee_auction_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_buyback_fee_liability = self
            .fees
            .interest_buyback_fee_liability
            .checked_add(buyback_auction_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_growth_index_q64 = self
            .fees
            .interest_growth_index_q64
            .checked_add(growth_delta)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_liability = self
            .fees
            .interest_liability
            .checked_add(lp_interest)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        // With no ordinary holder, burned MIN_LIQUIDITY is the complete sink.
        // Discard its sub-Q64 carry as well so a later depositor cannot inherit
        // any fraction of already-published funding interest.
        self.fees.hlp_funding_interest_growth_remainder_scaled = if eligible_srlp_supply == MIN_LIQUIDITY {
            0
        } else {
            remainder_scaled
        };
        self.fees.assert_backed()?;
        FeesReceipt::from_side(self)
    }

    pub fn settle_referral_interest_claim(&mut self, amount: u64, interest_vault_balance: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::AmountZero);
        self.fees.referral_interest_liability = self
            .fees
            .referral_interest_liability
            .checked_sub(amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        self.fees.interest_vault_balance = interest_vault_balance;
        self.fees.assert_backed()
    }

    pub fn carry_forward_swap_fees(&mut self) -> Result<()> {
        self.carry_forward_swap_fees_with_supply(self.shares.srlp_supply)
    }

    pub fn carry_forward_swap_fees_with_supply(&mut self, supply: u64) -> Result<()> {
        if supply == 0
            || (self.fees.unallocated_swap_fee_liability == 0 && self.fees.swap_fee_growth_remainder_scaled == 0)
        {
            return Ok(());
        }
        let allocated = self.fees.unallocated_swap_fee_liability;
        let senior = self
            .fees
            .junior
            .allocate(allocated, crate::transitions::tranches::TrancheRevenueSource::SwapFee)?;
        let supply = self.fees.junior.senior_supply(supply)?;
        let (growth_delta, remainder_scaled) =
            distribute_growth_q64(senior, supply, self.fees.swap_fee_growth_remainder_scaled)?;
        self.fees.swap_fee_growth_index_q64 = self
            .fees
            .swap_fee_growth_index_q64
            .checked_add(growth_delta)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.swap_fee_liability = self
            .fees
            .swap_fee_liability
            .checked_add(allocated)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.unallocated_swap_fee_liability = self
            .fees
            .unallocated_swap_fee_liability
            .checked_sub(allocated)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.swap_fee_growth_remainder_scaled = remainder_scaled;
        Ok(())
    }

    pub fn carry_forward_interest(&mut self) -> Result<()> {
        self.carry_forward_interest_with_supply(self.shares.srlp_supply)
    }

    pub fn carry_forward_interest_with_supply(&mut self, supply: u64) -> Result<()> {
        if supply == 0
            || (self.fees.unallocated_interest_liability == 0 && self.fees.interest_growth_remainder_scaled == 0)
        {
            return Ok(());
        }
        let allocated = self.fees.unallocated_interest_liability;
        let senior = self.fees.junior.allocate(
            allocated,
            crate::transitions::tranches::TrancheRevenueSource::BorrowInterest,
        )?;
        let supply = self.fees.junior.senior_supply(supply)?;
        let (growth_delta, remainder_scaled) =
            distribute_growth_q64(senior, supply, self.fees.interest_growth_remainder_scaled)?;
        self.fees.interest_growth_index_q64 = self
            .fees
            .interest_growth_index_q64
            .checked_add(growth_delta)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_liability = self
            .fees
            .interest_liability
            .checked_add(allocated)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.unallocated_interest_liability = self
            .fees
            .unallocated_interest_liability
            .checked_sub(allocated)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_growth_remainder_scaled = remainder_scaled;
        Ok(())
    }

    pub fn prepare_yield_claim(
        &mut self,
        yield_account: &mut YieldAccount,
        swap_fee_custody_balance: u64,
        interest_vault_balance: u64,
        holder_balance: u64,
    ) -> Result<YieldClaimReceipt> {
        self.carry_forward_swap_fees()?;
        self.carry_forward_interest()?;
        yield_account.accrue(
            holder_balance,
            self.fees.swap_fee_growth_index_q64,
            self.fees.interest_growth_index_q64,
        )?;
        let claim_amount = yield_account.claimable_amount()?;
        require!(claim_amount > 0, ErrorCode::AmountZero);
        require_gte!(
            swap_fee_custody_balance,
            yield_account.accrued_swap_fee_amount,
            ErrorCode::UnbackedFeeLiability
        );
        require_gte!(
            interest_vault_balance,
            yield_account.accrued_interest_amount,
            ErrorCode::UnbackedFeeLiability
        );
        Ok(YieldClaimReceipt {
            claim_amount,
            swap_fee_amount: yield_account.accrued_swap_fee_amount,
            interest_amount: yield_account.accrued_interest_amount,
            remaining_swap_fee_liability: self.fees.swap_fee_liability,
            remaining_interest_liability: self.fees.interest_liability,
        })
    }

    pub fn settle_yield_claim(
        &mut self,
        yield_account: &mut YieldAccount,
        claim_amount: u64,
        swap_fee_amount: u64,
        interest_amount: u64,
    ) -> Result<YieldClaimReceipt> {
        self.fees.swap_fee_liability = self
            .fees
            .swap_fee_liability
            .checked_sub(swap_fee_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_liability = self
            .fees
            .interest_liability
            .checked_sub(interest_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.swap_fee_custody_balance = self
            .fees
            .swap_fee_custody_balance
            .checked_sub(swap_fee_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_vault_balance = self
            .fees
            .interest_vault_balance
            .checked_sub(interest_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        yield_account.clear_claimed();
        self.fees.assert_backed()?;
        Ok(YieldClaimReceipt {
            claim_amount,
            swap_fee_amount,
            interest_amount,
            remaining_swap_fee_liability: self.fees.swap_fee_liability,
            remaining_interest_liability: self.fees.interest_liability,
        })
    }
}

impl Market {
    pub fn assert_market_invariants(&self) -> Result<()> {
        require_eq!(
            self.base_side.shares.srlp_supply,
            self.quote_side.shares.srlp_supply,
            ErrorCode::BrokenInvariant
        );
        self.tranches.validate(self.base_side.shares.srlp_supply)?;
        for side in [&self.base_side, &self.quote_side] {
            if side.fees.junior.initialized {
                require_eq!(
                    side.fees.junior.reserve_shares,
                    self.tranches.junior_reserve_shares,
                    ErrorCode::BrokenInvariant
                );
                require_eq!(
                    side.fees.junior.receipt_supply,
                    self.tranches.junior_receipt_supply,
                    ErrorCode::BrokenInvariant
                );
            }
        }
        self.base_side.assert_share_backing()?;
        self.quote_side.assert_share_backing()?;
        self.base_side.fees.assert_backed()?;
        self.quote_side.fees.assert_backed()?;
        if self.base_hlp_vault.hlp_supply == 0 {
            require_eq!(
                self.base_side.reserves.base_hlp_backing_inventory,
                0,
                ErrorCode::BrokenInvariant
            );
            require_eq!(
                self.quote_side.reserves.base_hlp_backing_inventory,
                0,
                ErrorCode::BrokenInvariant
            );
        }
        if self.quote_hlp_vault.hlp_supply == 0 {
            require_eq!(
                self.base_side.reserves.quote_hlp_backing_inventory,
                0,
                ErrorCode::BrokenInvariant
            );
            require_eq!(
                self.quote_side.reserves.quote_hlp_backing_inventory,
                0,
                ErrorCode::BrokenInvariant
            );
        }
        self.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        self.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
        Ok(())
    }

    pub fn assert_virtual_reserve_invariant(&self, asset: MarketAsset) -> Result<()> {
        let (side, cash_backed_debt) = match asset {
            MarketAsset::Base => (
                &self.base_side,
                total_cash_backed_borrowed(self, asset, self.debt.base_borrow_index_nad)?,
            ),
            MarketAsset::Quote => (
                &self.quote_side,
                total_cash_backed_borrowed(self, asset, self.debt.quote_borrow_index_nad)?,
            ),
        };
        let flash_collateral = match asset {
            MarketAsset::Base => self.base_hlp_vault.flash_collateral_debit,
            MarketAsset::Quote => self.quote_hlp_vault.flash_collateral_debit,
        };
        require_eq!(
            side.reserves.flash_receivable,
            flash_collateral,
            ErrorCode::BrokenInvariant
        );
        let hlp_live = self.hlp_live_reserve(asset)?;
        // Invariants:
        // 1. x_virtual * y_virtual = k (Constant product invariant)
        // 2. r_virtual >= r_cash_backed_debt (Solvency invariant)
        // with a state transition:
        // ΔR_virtual = ΔR_cash + ΔR_cash_backed_debt + ΔR_hlp_live.
        // hLP funding debt is priced through utilization and hLP NAV, but it is
        // not same-side cash-backed reserve debt.
        let expected_live_reserve = (side.reserves.cash_reserve as u128)
            .checked_add(cash_backed_debt)
            .and_then(|value| value.checked_add(u128::from(side.reserves.flash_receivable)))
            .and_then(|value| value.checked_add(hlp_live))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_eq!(
            side.reserves.live_reserve,
            expected_live_reserve,
            ErrorCode::BrokenInvariant
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SwapReceipt {
    pub amount_in_after_fee: u64,
    pub reserve_input_credit: u64,
    pub amount_out: u64,
    pub fee_credit: u64,
    pub base_fee_credit: u64,
    pub distributed_surcharge_credit: u64,
    pub fee_breakdown: SwapFeeBreakdown,
    pub reserve_in_live_reserve: u128,
    pub reserve_out_live_reserve: u128,
    pub fees: FeesReceipt,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeesReceipt {
    pub swap_fee_growth_index_q64: u128,
    pub interest_growth_index_q64: u128,
    pub swap_fee_liability: u64,
    pub interest_liability: u64,
    pub unallocated_swap_fee_liability: u64,
    pub unallocated_interest_liability: u64,
    pub referral_interest_liability: u64,
    pub protocol_fee_liability: u64,
    pub buyback_fee_liability: u64,
    pub swap_fee_custody_balance: u64,
    pub interest_vault_balance: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct YieldClaimReceipt {
    pub claim_amount: u64,
    pub swap_fee_amount: u64,
    pub interest_amount: u64,
    pub remaining_swap_fee_liability: u64,
    pub remaining_interest_liability: u64,
}

#[cfg(test)]
mod fees_tests {
    include!("../tests/transitions/ledger_fees.rs");
}

#[cfg(test)]
mod market_reserve_tests {
    include!("../tests/transitions/ledger_reserves.rs");
}

#[cfg(test)]
mod side_accounting_tests {
    include!("../tests/transitions/ledger_accounting.rs");
}

#[cfg(test)]
include!("../tests/support/ledger.rs");
