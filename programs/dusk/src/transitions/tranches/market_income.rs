use crate::{
    errors::ErrorCode,
    math::mul_div_u128,
    state::{IncomeExposureEnd, IncomeReceivableClaim, Market, MarketAsset, MarketIncomeBooks},
};
use anchor_lang::prelude::*;

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

    fn income_receivable_allocations(
        &self,
        asset: MarketAsset,
        funding: bool,
        amount: u64,
    ) -> Result<([(u64, u64); 4], u16)> {
        let i = asset.code() as usize + if funding { 2 } else { 0 };
        let supply = self.base_side.shares.srlp_supply;
        let senior_supply = self.tranches.senior_reserve_shares(supply)?;
        let base_hlp = if funding { 0 } else { self.base_hlp_vault.srlp_shares };
        let quote_hlp = if funding { 0 } else { self.quote_hlp_vault.srlp_shares };
        let excluded = if funding {
            self.base_hlp_vault
                .srlp_shares
                .checked_add(self.quote_hlp_vault.srlp_shares)
                .ok_or(ErrorCode::SupplyOverflow)?
        } else {
            0
        };
        let senior_supply = senior_supply.checked_sub(excluded).ok_or(ErrorCode::BrokenInvariant)?;
        require!(senior_supply > 0, ErrorCode::SupplyUnderflow);
        let split_carry = self.income.get()?.split_carry_bps[i];
        let numerator = (amount as u128)
            .checked_mul(u128::from(self.tranche_policy.junior_income_share_bps))
            .and_then(|value| value.checked_add(u128::from(split_carry)))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let junior = u64::try_from(numerator / 10_000).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let carry = u16::try_from(numerator % 10_000).map_err(|_| ErrorCode::MarketMathOverflow)?;
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
        Ok((
            [
                (senior - bh - qh, ordinary_supply),
                (junior, jr_supply),
                (bh, 1),
                (qh, 1),
            ],
            carry,
        ))
    }

    /// Check the same frozen fee and LP-unit issuance as settlement, without
    /// mutating ownership. A numerical epoch limit pauses new interest rather
    /// than blocking an existing borrower's repayment or writeoff.
    #[inline(never)]
    pub(crate) fn income_accrual_fits(&self, asset: MarketAsset, public: u64, funding: u64) -> Result<bool> {
        // An hLP exit materializes funded srLP backing plus its historical
        // coupon as raw atoms. Reserve enough range even if that vault owns
        // the entire unfunded public coupon; do not pause an already owed claim.
        let unfunded = public
            .checked_sub(self.side(asset).fees.public_interest_advance.outstanding_gross)
            .ok_or(ErrorCode::BrokenInvariant)?;
        for vault in [&self.base_hlp_vault, &self.quote_hlp_vault] {
            if vault.srlp_shares == 0 {
                continue;
            }
            let funded = mul_div_u128(
                self.curve_reserve(asset)? as u128,
                vault.srlp_shares as u128,
                self.base_side.shares.srlp_supply as u128,
            )?;
            if funded
                .checked_add(unfunded as u128)
                .is_none_or(|v| v > u64::MAX as u128)
            {
                return Ok(false);
            }
        }
        if !self.side(asset).fees.junior.initialized {
            return Ok(true);
        }
        for (offset, gross) in [(0, public), (2, funding)] {
            let i = asset.code() as usize + offset;
            let income = self.income.get()?;
            let mut revenue = *income.revenue[i];
            require_gte!(gross, revenue.gross_unpaid, ErrorCode::BrokenInvariant);
            revenue.synchronize(
                gross,
                0,
                self.protocol_interest_markup_bps,
                self.operator.interest_share_bps,
            )?;
            let advanced = if offset == 0 {
                let advance = self.side(asset).fees.public_interest_advance;
                advance
                    .outstanding_gross
                    .checked_sub(advance.revenue_escrow)
                    .ok_or(ErrorCode::BrokenInvariant)?
            } else {
                0
            };
            let expected = gross
                .checked_sub(revenue.unpaid_fees()?)
                .and_then(|v| v.checked_sub(advanced))
                .ok_or(ErrorCode::BrokenInvariant)?;
            let book = &income.books[i];
            let issued = expected.checked_sub(book.unpaid).ok_or(ErrorCode::BrokenInvariant)?;
            if issued == 0 {
                continue;
            }
            require_eq!(income.pending[i], 0, ErrorCode::BrokenInvariant);
            let (allocations, _) = self.income_receivable_allocations(asset, offset != 0, issued)?;
            if book.project_issue(allocations)?.is_none() {
                return Ok(false);
            }
        }
        Ok(true)
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
        let (allocations, carry) = self.income_receivable_allocations(asset, funding, amount)?;
        self.income.get_mut()?.split_carry_bps[i] = carry;
        self.income.get_mut()?.books[i].issue(allocations)?;
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
    /// permits a later cash credit; principal recovery is not yield.
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
        let gross = if funding {
            let vault = if asset == MarketAsset::Base {
                &self.quote_hlp_vault
            } else {
                &self.base_hlp_vault
            };
            crate::state::Debt::shares_to_debt(vault.debt_shares, self.debt.borrow_index(asset))?
                .saturating_sub(vault.debt_principal as u128)
        } else {
            self.public_unpaid_interest(asset)?
        };
        let gross = u64::try_from(gross).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let i = asset.code() as usize + if funding { 2 } else { 0 };
        let advance = self.side(asset).fees.public_interest_advance;
        let old_gross = self.income.get()?.revenue[i].gross_unpaid;
        let paid = if gross < old_gross {
            payable
                .checked_add(if funding { 0 } else { advance.repayment_credit_pending })
                .ok_or(ErrorCode::MarketMathOverflow)?
        } else {
            0
        };
        let markup = self.protocol_interest_markup_bps;
        let operator_share = self.operator.interest_share_bps;
        let paid_fee = if gross == old_gross {
            // Same-slot scratch quotes share immutable income snapshots. Do
            // not clone every nested book merely to synchronize identical fees.
            require_eq!(paid, 0, ErrorCode::BrokenInvariant);
            0
        } else {
            self.income.get_mut()?.revenue[i].synchronize(gross, paid, markup, operator_share)?
        };
        let fee_unpaid = self.income.get()?.revenue[i].unpaid_fees()?;
        let advanced_lp = if funding {
            0
        } else {
            advance
                .outstanding_gross
                .checked_sub(advance.revenue_escrow)
                .ok_or(ErrorCode::BrokenInvariant)?
        };
        let expected = gross
            .checked_sub(fee_unpaid)
            .and_then(|v| v.checked_sub(advanced_lp))
            .ok_or(ErrorCode::BrokenInvariant)?;
        let payable = if gross < old_gross {
            payable
                .checked_add(if funding { 0 } else { advance.revenue_release_pending })
                .and_then(|v| v.checked_sub(paid_fee))
                .ok_or(ErrorCode::BrokenInvariant)?
        } else {
            payable
        };
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

    /// Quote from frozen fee ownership, including previously funded fractional
    /// carry. Current Futarchy or operator rates never re-price old interest.
    pub(crate) fn quote_interest_revenue_credit(
        &self,
        asset: MarketAsset,
        funding: bool,
        credit: u64,
    ) -> Result<crate::transitions::InterestRevenueCredit> {
        let i = asset.code() as usize + if funding { 2 } else { 0 };
        let income = self.income.get()?;
        let total = income.pending[i]
            .checked_add(income.revenue[i].pending_gross)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let (fee, protocol) = income.revenue[i].quote_cash(credit, total)?;
        let available = self
            .side(asset)
            .fees
            .revenue_interest_liability
            .checked_add(fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(available, protocol, ErrorCode::UnbackedFeeLiability);
        Ok(crate::transitions::InterestRevenueCredit {
            lp: credit - fee,
            fee,
            protocol,
            available_fee_custody: available,
        })
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
        let quote = self.quote_interest_revenue_credit(asset, funding, credit)?;
        require_eq!(protocol, quote.protocol, ErrorCode::BrokenInvariant);
        let i = asset.code() as usize + if funding { 2 } else { 0 };
        let recorded = self.income.get_mut()?.revenue[i].collect(quote.fee)?;
        require_eq!(recorded, protocol, ErrorCode::BrokenInvariant);
        self.collect_income_receivable(asset, funding, quote.lp)?;
        let fees = &mut self.side_mut(asset).fees;
        fees.interest_vault_balance = fees
            .interest_vault_balance
            .checked_add(credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        fees.interest_liability = fees
            .interest_liability
            .checked_add(quote.lp)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        fees.revenue_interest_liability = quote
            .available_fee_custody
            .checked_sub(protocol)
            .ok_or(ErrorCode::UnbackedFeeLiability)?;
        fees.credit_protocol_interest_revenue(protocol, split, referral)?;
        fees.assert_backed()
    }
}
