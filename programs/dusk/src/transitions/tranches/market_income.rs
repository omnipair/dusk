use super::{IncomeReceivableBook, IncomeReceivableClaim, ReceivableGrowth};
use crate::{
    errors::ErrorCode,
    math::mul_div_u128,
    state::{Market, MarketAsset},
};
use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace)]
pub struct IncomeExposureEnd {
    pub ended: bool,
    pub epoch: u64,
    pub growth: ReceivableGrowth,
}

/// Public-loan and hLP-funding receivables are independent. The vault-owned
/// public-income claims preserve hLP's automatic funding-gap repayment.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Default, InitSpace)]
pub struct MarketIncomeBooks {
    /// base public, quote public, base hLP funding, quote hLP funding.
    pub books: [Box<IncomeReceivableBook>; 4],
    /// Gross settlement budget awaiting measured net custody credit.
    pub pending: [u64; 4],
    pub seed: [IncomeReceivableClaim; 4],
    /// [hLP target][income asset], owned by the vault rather than its holders.
    pub hlp: [[IncomeReceivableClaim; 2]; 2],
    pub junior_end: [IncomeExposureEnd; 4],
    pub split_carry: [bool; 4],
}
impl anchor_lang::Space for Box<MarketIncomeBooks> {
    const INIT_SPACE: usize = MarketIncomeBooks::INIT_SPACE;
}

impl Market {
    /// The vault owns these receivables within senior's half of public income.
    /// They remain in hLP NAV until collection; ordinary LP historical claims
    /// and junior's allocation are not collateral belonging to this vault.
    pub(crate) fn hlp_unpaid_income(&self, target: MarketAsset, asset: MarketAsset) -> Result<u64> {
        if self.unrealized_interest(asset)? == 0 {
            return Ok(0);
        }
        let income = self.income.get()?;
        let i = asset.code() as usize;
        income.hlp[target.code() as usize][i].unpaid_value(&income.books[i])
    }

    /// Exchange only this vault's unpaid coupon for pool backing. The reserve
    /// acquires the borrower receivable at its existing gross NAV; future
    /// repayment restores the reserve rather than paying the coupon twice.
    /// No token transfer occurs here. Exit/rehedge settles the matching capital
    /// and debt legs atomically and still enforces actual cash availability.
    pub(crate) fn capitalize_hlp_receivable(
        &mut self,
        target: MarketAsset,
        asset: MarketAsset,
        numerator: u64,
        denominator: u64,
    ) -> Result<u64> {
        if !self.side(asset).fees.junior.initialized {
            return Ok(0);
        }
        let unpaid = self.public_unpaid_interest(asset)?;
        let income = self.income.get_mut()?;
        let i = asset.code() as usize;
        let claim = &mut income.hlp[target.code() as usize][i];
        claim.checkpoint(1, &income.books[i], target.code() as usize + 2)?;
        let value = claim.redeem_unpaid_units(&mut income.books[i], numerator, denominator)?;
        self.side_mut(asset)
            .fees
            .public_interest_advance
            .fund(unpaid, value, 0)?;
        Ok(value)
    }

    pub(crate) fn quote_hlp_capitalization_for_value(
        &self,
        target: MarketAsset,
        needed: u64,
    ) -> Result<(u64, u64, u64)> {
        let income = self.income.get()?;
        let i = target.code() as usize;
        let book = &income.books[i];
        let claim = income.hlp[i][i];
        require!(book.unpaid > 0, ErrorCode::BrokenInvariant);
        let units = u64::try_from(crate::math::mul_div_ceil_u128(
            needed as u128,
            book.units as u128,
            book.unpaid as u128,
        )?)
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        let owned = (claim.units_q64 >> 64) as u64;
        require_gte!(owned, units, ErrorCode::BrokenInvariant);
        let value = self.quote_hlp_receivable_conversion(target, target, units, owned)?;
        require_gte!(value, needed, ErrorCode::BrokenInvariant);
        Ok((units, owned, value))
    }

    /// A fully closed hLP cohort cannot gift rounding remnants or collected
    /// income to a later depositor. Whole cash goes to the closing holders;
    /// sub-atom coupon/cash fractions are donated to the permanent seed.
    pub(crate) fn retire_hlp_receivables(&mut self, target: MarketAsset) -> Result<()> {
        if !self.base_side.fees.junior.initialized {
            return Ok(());
        }
        self.checkpoint_seed_receivables()?;
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let i = asset.code() as usize;
            let t = target.code() as usize;
            let income = self.income.get_mut()?;
            let mut old = income.hlp[t][i];
            old.checkpoint(1, &income.books[i], t + 2)?;
            require!(old.unpaid_value(&income.books[i])? <= 1, ErrorCode::BrokenInvariant);
            let seed = &mut income.seed[i];
            seed.units_q64 = seed
                .units_q64
                .checked_add(old.units_q64)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let (fraction, carry) = seed.paid_fraction_q128.overflowing_add(old.paid_fraction_q128);
            seed.paid_fraction_q128 = fraction;
            seed.claimable = seed
                .claimable
                .checked_add(u64::from(carry))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            income.hlp[t][i] = IncomeReceivableClaim {
                epoch: income.books[i].epoch,
                paid_checkpoint_q64: income.books[i].paid_per_unit_q64,
                growth_checkpoint: income.books[i].growth[t + 2],
                ..IncomeReceivableClaim::default()
            };
            let vault = if t == 0 {
                &mut self.base_hlp_vault
            } else {
                &mut self.quote_hlp_vault
            };
            vault.publish_new_interest(asset, old.claimable)?;
        }
        Ok(())
    }

    /// Exact value available for a proportional conversion, rounded down to
    /// whole receivable units. Quotes and settlement use the same rounding.
    pub(crate) fn quote_hlp_receivable_conversion(
        &self,
        target: MarketAsset,
        asset: MarketAsset,
        numerator: u64,
        denominator: u64,
    ) -> Result<u64> {
        if !self.side(asset).fees.junior.initialized || self.unrealized_interest(asset)? == 0 {
            return Ok(0);
        }
        let income = self.income.get()?;
        let i = asset.code() as usize;
        let mut book = (*income.books[i]).clone();
        let mut claim = income.hlp[target.code() as usize][i];
        claim.checkpoint(1, &book, target.code() as usize + 2)?;
        claim.redeem_unpaid_units(&mut book, numerator, denominator)
    }

    pub fn checkpoint_seed_receivables(&mut self) -> Result<()> {
        let balance = if self.tranches.junior_reserve_shares == 0 {
            1
        } else {
            self.tranches.permanent_receipts
        };
        let income = self.income.get_mut()?;
        for i in 0..4 {
            income.seed[i].checkpoint(balance, &income.books[i], 1)?;
        }
        Ok(())
    }

    /// Freeze exposure before capital or vault share ownership changes. Each
    /// hLP lane has one owner (the vault); ordinary senior and junior units
    /// are distributed over their respective receipt supplies.
    pub fn issue_income_receivable(&mut self, asset: MarketAsset, funding: bool, amount: u64) -> Result<()> {
        if amount == 0 || !self.side(asset).fees.junior.initialized {
            return Ok(());
        }
        let i = asset.code() as usize + if funding { 2 } else { 0 };
        require_eq!(self.income.get()?.pending[i], 0, ErrorCode::BrokenInvariant);
        let supply = self.base_side.shares.ylp_supply;
        let senior_supply = self.tranches.senior_reserve_shares(supply)?;
        let base_hlp = if funding { 0 } else { self.base_hlp_vault.ylp_shares };
        let quote_hlp = if funding { 0 } else { self.quote_hlp_vault.ylp_shares };
        let excluded = if funding {
            self.base_hlp_vault
                .ylp_shares
                .checked_add(self.quote_hlp_vault.ylp_shares)
                .ok_or(ErrorCode::SupplyOverflow)?
        } else {
            0
        };
        let senior_supply = senior_supply.checked_sub(excluded).ok_or(ErrorCode::BrokenInvariant)?;
        require!(senior_supply > 0, ErrorCode::SupplyUnderflow);
        let numerator = amount as u128 + u128::from(self.income.get()?.split_carry[i]);
        let junior = (numerator / 2) as u64;
        self.income.get_mut()?.split_carry[i] = !numerator.is_multiple_of(2);
        let senior = amount - junior;
        let bh = mul_div_u128(senior as u128, base_hlp as u128, senior_supply as u128)? as u64;
        let qh = mul_div_u128(senior as u128, quote_hlp as u128, senior_supply as u128)? as u64;
        let ordinary_supply = senior_supply
            .checked_sub(base_hlp)
            .and_then(|v| v.checked_sub(quote_hlp))
            .ok_or(ErrorCode::BrokenInvariant)?;
        let jr_supply = if self.tranches.junior_reserve_shares == 0 {
            1
        } else {
            self.tranches.junior_receipt_supply
        };
        self.income.get_mut()?.books[i].issue([
            (senior - bh - qh, ordinary_supply),
            (junior, jr_supply),
            (bh, 1),
            (qh, 1),
        ])?;
        // Vault receipt supply is exactly one. Accrued units remain vault
        // property across hLP entry/exit, valued in its existing NAV.
        if !funding {
            let income = self.income.get_mut()?;
            for target in 0..2 {
                income.hlp[target][asset.code() as usize].checkpoint(1, &income.books[i], target + 2)?;
            }
        }
        let seed_balance = if self.tranches.junior_reserve_shares == 0 {
            1
        } else {
            self.tranches.permanent_receipts
        };
        let income = self.income.get_mut()?;
        income.seed[i].checkpoint(seed_balance, &income.books[i], 1)
    }

    /// Remove both paid and written-off receivables. Only the paid portion
    /// permits a later cash credit; insurance-funded principal is not yield.
    pub fn clear_income_receivable(
        &mut self,
        asset: MarketAsset,
        funding: bool,
        removed: u64,
        payable: u64,
    ) -> Result<()> {
        if !self.side(asset).fees.junior.initialized {
            return Ok(());
        }
        require_gte!(removed, payable, ErrorCode::BrokenInvariant);
        let i = asset.code() as usize + if funding { 2 } else { 0 };
        self.income.get_mut()?.books[i].settle(removed, 0)?;
        self.income.get_mut()?.pending[i] = self.income.get()?.pending[i]
            .checked_add(payable)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }

    pub fn collect_income_receivable(&mut self, asset: MarketAsset, funding: bool, cash: u64) -> Result<()> {
        let i = asset.code() as usize + if funding { 2 } else { 0 };
        require_gte!(self.income.get()?.pending[i], cash, ErrorCode::UnbackedFeeLiability);
        self.income.get_mut()?.books[i].credit_cash(cash)?;
        self.income.get_mut()?.pending[i] = 0;
        Ok(())
    }
}

impl Market {
    /// Checkpoint before every senior/junior balance change. Historical units
    /// remain with the owner even when their wallet and exit escrow reach zero.
    pub fn checkpoint_holder_income(
        &self,
        holder: &mut crate::state::YieldAccount,
        wallet_balance: u64,
        ended: Option<[IncomeExposureEnd; 4]>,
    ) -> Result<()> {
        if holder.token_kind == crate::state::YieldTokenKind::Hlp.code() {
            return Ok(());
        }
        let asset = self.asset_for_mint(holder.asset_mint)?;
        let junior = holder.token_kind == crate::state::YieldTokenKind::Junior.code();
        let lane = usize::from(junior);
        let ends = ended.unwrap_or(self.income.get()?.junior_end);
        let balance = wallet_balance
            .checked_add(holder.escrow_balance)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        for source in 0..2 {
            let i = asset.code() as usize + source * 2;
            let book = &self.income.get()?.books[i];
            let claim = &mut holder.income[source];
            if !holder.income_initialized {
                claim.epoch = book.epoch;
                claim.checkpoint_exposure(
                    0,
                    book,
                    lane,
                    if junior { ends[i] } else { IncomeExposureEnd::default() },
                )?;
            } else {
                claim.checkpoint_exposure(
                    balance,
                    book,
                    lane,
                    if junior { ends[i] } else { IncomeExposureEnd::default() },
                )?;
            }
            holder.accrued_interest_amount = holder
                .accrued_interest_amount
                .checked_add(claim.claimable)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            claim.claimable = 0;
        }
        holder.income_initialized = true;
        Ok(())
    }
}

impl Market {
    pub fn sync_income_receivable(&mut self, asset: MarketAsset, funding: bool, payable: u64) -> Result<()> {
        if !self.side(asset).fees.junior.initialized {
            return Ok(());
        }
        let expected = if funding {
            let vault = if asset == MarketAsset::Base {
                &self.quote_hlp_vault
            } else {
                &self.base_hlp_vault
            };
            crate::state::Debt::shares_to_debt(vault.debt_shares, self.debt.borrow_index(asset))?
                .saturating_sub(vault.debt_principal as u128)
        } else {
            self.public_unpaid_interest(asset)?
                .checked_sub(self.side(asset).fees.public_interest_advance.outstanding_gross as u128)
                .ok_or(ErrorCode::BrokenInvariant)?
        };
        let expected = u64::try_from(expected).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let i = asset.code() as usize + if funding { 2 } else { 0 };
        let previous = self.income.get()?.books[i].unpaid;
        if expected == previous {
            require_eq!(payable, 0, ErrorCode::BrokenInvariant);
            return Ok(());
        }
        if expected > previous {
            require_eq!(payable, 0, ErrorCode::BrokenInvariant);
            self.issue_income_receivable(asset, funding, expected - previous)
        } else {
            self.clear_income_receivable(asset, funding, previous - expected, payable)
        }
    }

    pub(crate) fn record_receivable_income_credit(
        &mut self,
        asset: MarketAsset,
        funding: bool,
        credit: u64,
        protocol: u64,
        split: crate::state::ProtocolAuctionSplit,
        referral: u64,
    ) -> Result<()> {
        let lp = self
            .side_mut(asset)
            .record_interest_custody(credit, protocol, split, referral)?;
        self.collect_income_receivable(asset, funding, lp)?;
        let fees = &mut self.side_mut(asset).fees;
        fees.interest_liability = fees
            .interest_liability
            .checked_add(lp)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        fees.assert_backed()
    }
}
