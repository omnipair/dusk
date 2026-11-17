//! Accrued-interest ledger. Borrower interest is credited to holders when it
//! accrues and claims are paid from reserve cash.
//!
//! Per asset, with `C = cash_reserve + flash_receivable`, `D` borrower
//! principal, `I = public_recognized + funding_recognized`, `Q = credited`,
//! `Pu = protocol_unpaid`, `Δ = deficit` and `P = curve_reserve`:
//!
//! ```text
//! C + D + I + Δ = P + Q + Pu
//! curve_reserve = max(0, live_reserve + funding_recognized + deficit − credited − protocol_unpaid)
//! ```
//!
//! Accrual, repayment, netting and claims leave `curve_reserve` unchanged. A
//! write-off reduces it by the LP and operator part, which the junior-first
//! waterfall absorbs; the protocol part leaves `protocol_unpaid`. The part
//! of a loss beyond LP capital is recorded in `deficit`.
use crate::{
    constants::{BPS_DENOMINATOR, NAD},
    errors::ErrorCode,
    math::{distribute_growth_q64, mul_div_ceil_u128, mul_div_u128},
    state::{Debt, Fees, HlpVault, InterestLedger, Market, MarketAsset, MarketSide, ProtocolAuctionSplit},
    transitions::tranches::TrancheRevenueSource,
};
use anchor_lang::prelude::*;

/// Recognized interest sources of one asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InterestSource {
    /// Fixed and isolated borrower interest.
    Public,
    /// hLP funding interest owed by the vault that borrows the asset.
    Funding,
}

/// How a payment's protocol share leaves reserve cash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProtocolCash {
    /// Cash must cover the share (`CashReserveUnderflow` otherwise).
    Required,
    /// A spot swap applying a prepared plan: pay what cash allows. The
    /// forgone share stays in LP principal.
    IfCashAllows,
}

/// The held interest of a frozen vault a terminal close applies to its
/// debt, and the borrowed-asset recovery it is worth.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HeldInterestUse {
    pub target: u64,
    pub borrowed: u64,
    pub value: u64,
}

/// Split of newly recognized interest, for the accrual event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InterestCredit {
    pub protocol: u64,
    pub operator: u64,
    /// LP share: junior lane (or seed), ordinary srLP and both hLP vaults.
    pub lp: u64,
    /// hLP vault public interest netted against its funding debt.
    pub netted: u64,
}

impl InterestCredit {
    fn add(&mut self, other: Self) {
        self.protocol = self.protocol.saturating_add(other.protocol);
        self.operator = self.operator.saturating_add(other.operator);
        self.lp = self.lp.saturating_add(other.lp);
        self.netted = self.netted.saturating_add(other.netted);
    }
}

/// Result of removing outstanding interest by payment or write-off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InterestSettlement {
    /// Protocol share of the payment moved from cash to `protocol_pending`.
    pub protocol_paid: u64,
    /// Interest removed without payment.
    pub written_off: u64,
    /// Protocol share of the write-off, dropped from `protocol_unpaid`.
    pub protocol_written_off: u64,
    /// LP and operator part of the write-off: the principal loss to absorb.
    pub lp_loss: u64,
}

/// Carry fractional atoms across changing denominators without floating point.
pub(crate) fn split_with_fraction(amount: u64, numerator: u64, denominator: u64, fraction: &mut u64) -> Result<u64> {
    require!(denominator > 0 && numerator <= denominator, ErrorCode::BrokenInvariant);
    let product = amount as u128 * numerator as u128;
    let whole = product / denominator as u128;
    let fractional = ((product % denominator as u128) << 64) / denominator as u128 + *fraction as u128;
    *fraction = fractional as u64;
    u64::try_from(whole + (fractional >> 64)).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn checked_u64(value: u128) -> Result<u64> {
    u64::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

impl InterestLedger {
    /// Recognized interest outstanding, `I`.
    pub(crate) fn interest(&self) -> Result<u128> {
        self.public_recognized
            .checked_add(self.funding_recognized)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    /// Protocol shares `(paid, written off)` of removing `removed` outstanding
    /// interest of which `paid` was paid. The paid share rounds down and the
    /// written-off share up; when every outstanding atom leaves, the protocol
    /// bucket drains exactly.
    fn protocol_shares(&self, removed: u128, paid: u64) -> Result<(u128, u128)> {
        require!(u128::from(paid) <= removed, ErrorCode::BrokenInvariant);
        let interest = self.interest()?;
        if removed == 0 || interest == 0 {
            return Ok((0, 0));
        }
        let unpaid = self.protocol_unpaid;
        let written_off = removed - u128::from(paid);
        if removed == interest {
            let written = mul_div_ceil_u128(written_off, unpaid, interest)?;
            return Ok((unpaid - written, written));
        }
        let paid_share = mul_div_u128(u128::from(paid), unpaid, interest)?;
        let written = mul_div_ceil_u128(written_off, unpaid, interest)?.min(unpaid - paid_share);
        Ok((paid_share, written))
    }

    /// Protocol share of a payment on this ledger: `floor(paid · Pu / I)`.
    pub(crate) fn protocol_share(&self, paid: u64) -> Result<u64> {
        checked_u64(self.protocol_shares(u128::from(paid), paid)?.0)
    }

    /// An upper bound on the protocol share a later settlement of `paid`
    /// takes from cash: `ceil(paid · Pu / I)`. A funding payment settled
    /// first leaves at most one atom more for the same payment.
    pub(crate) fn protocol_share_bound(&self, paid: u64) -> Result<u64> {
        let interest = self.interest()?;
        if interest == 0 {
            return Ok(0);
        }
        checked_u64(mul_div_ceil_u128(u128::from(paid), self.protocol_unpaid, interest)?.min(self.protocol_unpaid))
    }

    /// Remove `removed` recognized interest of `source`, `paid` of it paid.
    /// Returns the protocol shares `(paid, written off)` dropped from
    /// `protocol_unpaid`. Cash is the caller's.
    pub(crate) fn settle(&mut self, source: InterestSource, removed: u128, paid: u64) -> Result<(u128, u128)> {
        let (protocol_paid, protocol_written_off) = self.protocol_shares(removed, paid)?;
        let recognized = match source {
            InterestSource::Public => &mut self.public_recognized,
            InterestSource::Funding => &mut self.funding_recognized,
        };
        *recognized = recognized.checked_sub(removed).ok_or(ErrorCode::BrokenInvariant)?;
        self.protocol_unpaid -= protocol_paid + protocol_written_off;
        Ok((protocol_paid, protocol_written_off))
    }
}

impl Market {
    /// The hLP vault whose funding debt is denominated in `asset`: the quote
    /// vault borrows base and the base vault borrows quote.
    pub(crate) fn funding_vault(&self, asset: MarketAsset) -> &HlpVault {
        match asset {
            MarketAsset::Base => &self.quote_hlp_vault,
            MarketAsset::Quote => &self.base_hlp_vault,
        }
    }

    fn hlp_vault_mut(&mut self, target: MarketAsset) -> &mut HlpVault {
        match target {
            MarketAsset::Base => &mut self.base_hlp_vault,
            MarketAsset::Quote => &mut self.quote_hlp_vault,
        }
    }

    /// The part of a frozen vault's held interest a terminal close applies
    /// to `shortfall`, the borrowed-asset debt the close cannot otherwise
    /// recover: borrowed-asset interest first, then target-asset interest
    /// valued at the terminal EMA mark both closes use for the vault's
    /// collateral. Its value never exceeds the shortfall, so held interest
    /// never creates a surplus.
    pub(crate) fn plan_held_hlp_interest(
        &self,
        target: MarketAsset,
        shortfall: u64,
        current_slot: u64,
    ) -> Result<HeldInterestUse> {
        let vault = match target {
            MarketAsset::Base => &self.base_hlp_vault,
            MarketAsset::Quote => &self.quote_hlp_vault,
        };
        let (held_target, held_borrowed) = vault.held_interest(target);
        let borrowed = held_borrowed.min(shortfall);
        let rest = shortfall - borrowed;
        if rest == 0 || held_target == 0 {
            return Ok(HeldInterestUse {
                target: 0,
                borrowed,
                value: borrowed,
            });
        }
        let full = self.hlp_ema_collateral_value_in_borrowed(target, held_target, 0, current_slot)?;
        let (used, value) = if full <= rest {
            (held_target, full)
        } else {
            let used = checked_u64(mul_div_u128(
                u128::from(held_target),
                u128::from(rest),
                u128::from(full),
            )?)?;
            (
                used,
                self.hlp_ema_collateral_value_in_borrowed(target, used, 0, current_slot)?,
            )
        };
        Ok(HeldInterestUse {
            target: used,
            borrowed,
            value: borrowed.checked_add(value).ok_or(ErrorCode::MarketMathOverflow)?,
        })
    }

    /// Terminal close: the held interest `used` covers leaves `credited` and
    /// stays in the pool as recovery; the rest is published to the vault's
    /// holders in its own asset. Neither step can fail on cash.
    pub(crate) fn settle_held_hlp_interest(&mut self, target: MarketAsset, used: HeldInterestUse) -> Result<()> {
        let vault = self.hlp_vault_mut(target);
        let (held_target, held_borrowed) = vault.held_interest(target);
        require!(
            used.target <= held_target && used.borrowed <= held_borrowed,
            ErrorCode::BrokenInvariant
        );
        vault.held_base_interest = 0;
        vault.held_quote_interest = 0;
        let borrowed = target.opposite();
        vault.publish_new_interest(target, held_target - used.target)?;
        vault.publish_new_interest(borrowed, held_borrowed - used.borrowed)?;
        for (asset, amount) in [(target, used.target), (borrowed, used.borrowed)] {
            let ledger = &mut self.side_mut(asset).fees.ledger;
            ledger.credited = ledger
                .credited
                .checked_sub(u128::from(amount))
                .ok_or(ErrorCode::BrokenInvariant)?;
        }
        Ok(())
    }

    /// Indexed hLP funding interest outstanding in `asset`, including carried
    /// funding: the borrowing vault's debt above its principal.
    pub(crate) fn funding_unpaid_interest(&self, asset: MarketAsset) -> Result<u128> {
        let vault = self.funding_vault(asset);
        if vault.debt_shares == 0 {
            return Ok(0);
        }
        Ok(self
            .hlp_funding_debt(asset)?
            .saturating_sub(u128::from(vault.debt_principal)))
    }

    /// Ledger terms excluded from the live reserve to form `curve_reserve`.
    pub(crate) fn curve_exclusion(&self, asset: MarketAsset) -> Result<i128> {
        let ledger = &self.side(asset).fees.ledger;
        let signed = |value: u128| i128::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow);
        signed(ledger.credited)?
            .checked_add(signed(ledger.protocol_unpaid)?)
            .and_then(|value| value.checked_sub(signed(ledger.funding_recognized).ok()?))
            .and_then(|value| value.checked_sub(signed(ledger.deficit).ok()?))
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    /// A market with a deficit admits no new LP capital: it would fund
    /// claims credited before it arrived.
    pub(crate) fn require_no_interest_deficit(&self) -> Result<()> {
        require!(!self.curve_wiped(), ErrorCode::InterestDeficit);
        Ok(())
    }

    /// Split newly recognized interest: protocol into `protocol_unpaid`,
    /// operator into `operator_claimable`, LP share to the junior lane (or
    /// seed), the hLP vaults (public only) and ordinary srLP. Every term
    /// except the protocol share enters `credited`. With `net`, the vault
    /// borrowing `asset` nets its public credit against its funding debt;
    /// only accrual nets, so a prepared hLP plan never sees its vault debt
    /// change between preparation and consumption.
    #[inline(never)]
    fn credit_interest(
        &mut self,
        asset: MarketAsset,
        source: InterestSource,
        gross: u128,
        net: bool,
    ) -> Result<InterestCredit> {
        let mut receipt = InterestCredit::default();
        let mut remaining = gross;
        // Each debt bucket is at most u64 under the accrual clip, so public
        // interest arrives in at most a few u64-sized chunks.
        while remaining > 0 {
            let chunk = remaining.min(u128::from(u64::MAX));
            remaining -= chunk;
            receipt.add(self.credit_interest_chunk(asset, source, chunk as u64, net)?);
        }
        Ok(receipt)
    }

    #[inline(never)]
    fn credit_interest_chunk(
        &mut self,
        asset: MarketAsset,
        source: InterestSource,
        gross: u64,
        net: bool,
    ) -> Result<InterestCredit> {
        let markup = self.protocol_interest_markup_bps;
        let operator_share = self.operator.interest_share_bps;
        require!(
            markup <= BPS_DENOMINATOR && operator_share <= crate::state::MAX_OPERATOR_INTEREST_SHARE_BPS,
            ErrorCode::InvalidInterestFeeBps
        );
        let senior_supply = self.tranches.senior_reserve_shares(self.base_side.shares.srlp_supply)?;
        let vault_shares = [self.base_hlp_vault.srlp_shares, self.quote_hlp_vault.srlp_shares];
        let fees = &mut self.side_mut(asset).fees;
        let ledger = &mut fees.ledger;
        let protocol = split_with_fraction(
            gross,
            u64::from(markup),
            u64::from(BPS_DENOMINATOR) + u64::from(markup),
            &mut ledger.protocol_fraction_q64,
        )?;
        let operator = split_with_fraction(
            gross - protocol,
            u64::from(operator_share),
            u64::from(BPS_DENOMINATOR),
            &mut ledger.operator_fraction_q64,
        )?;
        let lp = gross - protocol - operator;
        ledger.protocol_unpaid = ledger
            .protocol_unpaid
            .checked_add(u128::from(protocol))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        ledger.operator_claimable = ledger
            .operator_claimable
            .checked_add(u128::from(operator))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        ledger.credited = ledger
            .credited
            .checked_add(u128::from(gross - protocol))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let recognized = match source {
            InterestSource::Public => &mut ledger.public_recognized,
            InterestSource::Funding => &mut ledger.funding_recognized,
        };
        *recognized = recognized
            .checked_add(u128::from(gross))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let senior = fees.junior.allocate(lp, TrancheRevenueSource::BorrowInterest)?;
        // Funding interest goes to ordinary srLP and junior only; public
        // interest also credits the vault-held srLP pro rata.
        let vault_credit = if source == InterestSource::Public && senior_supply > 0 {
            [
                checked_u64(mul_div_u128(
                    u128::from(senior),
                    u128::from(vault_shares[0]),
                    u128::from(senior_supply),
                )?)?,
                checked_u64(mul_div_u128(
                    u128::from(senior),
                    u128::from(vault_shares[1]),
                    u128::from(senior_supply),
                )?)?,
            ]
        } else {
            [0, 0]
        };
        let ordinary = senior
            .checked_sub(vault_credit[0])
            .and_then(|value| value.checked_sub(vault_credit[1]))
            .ok_or(ErrorCode::BrokenInvariant)?;
        if ordinary > 0 {
            let ordinary_supply = senior_supply
                .checked_sub(vault_shares[0])
                .and_then(|value| value.checked_sub(vault_shares[1]))
                .ok_or(ErrorCode::BrokenInvariant)?;
            let (growth, carry) =
                distribute_growth_q64(ordinary, ordinary_supply, fees.interest_growth_remainder_scaled)?;
            fees.interest_growth_index_q64 = fees
                .interest_growth_index_q64
                .checked_add(growth)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            fees.interest_growth_remainder_scaled = carry;
        }
        let mut receipt = InterestCredit {
            protocol,
            operator,
            lp,
            netted: 0,
        };
        if source == InterestSource::Public {
            // The vault borrowing `asset` first nets its credit against the
            // funding it owes in `asset`; the other vault publishes in full.
            let borrower = asset.opposite();
            let borrower_credit = vault_credit[borrower.code() as usize];
            let netted = if net {
                self.net_vault_funding(asset, borrower_credit)?
            } else {
                0
            };
            receipt.netted = netted;
            // A frozen vault owes its creditors more than it holds: its
            // credit stays with it for the terminal waterfall instead of
            // reaching its holders. Either way it is in `credited`, so the
            // curve does not move.
            for (target, credit) in [
                (borrower, borrower_credit - netted),
                (asset, vault_credit[asset.code() as usize]),
            ] {
                let frozen = self.hlp_frozen(target);
                let vault = self.hlp_vault_mut(target);
                if frozen {
                    let held = match asset {
                        MarketAsset::Base => &mut vault.held_base_interest,
                        MarketAsset::Quote => &mut vault.held_quote_interest,
                    };
                    *held = held.checked_add(credit).ok_or(ErrorCode::MarketMathOverflow)?;
                } else {
                    vault.publish_new_interest(asset, credit)?;
                }
            }
        }
        Ok(receipt)
    }

    /// Net a vault's newly credited public interest against the funding it
    /// owes in the same asset by burning its debt shares. Neither the vault's
    /// principal nor its recorded live backing changes, so pricing does not
    /// move. Netting is skipped when cash cannot cover its protocol share.
    #[inline(never)]
    fn net_vault_funding(&mut self, asset: MarketAsset, credit: u64) -> Result<u64> {
        if credit == 0 {
            return Ok(0);
        }
        let ledger = self.side(asset).fees.ledger;
        let owed = u128::from(credit).min(ledger.funding_recognized);
        let vault = self.funding_vault(asset);
        if owed == 0 || vault.debt_shares == 0 {
            return Ok(0);
        }
        let index = self.debt.borrow_index(asset);
        let debt = Debt::shares_to_debt(vault.debt_shares, index)?;
        let target = debt.saturating_sub(owed).max(u128::from(vault.debt_principal));
        // Smallest remaining share count whose debt still covers `target`.
        let remaining = if target == 0 {
            0
        } else {
            mul_div_ceil_u128(target, NAD as u128, index)?
        };
        if remaining >= vault.debt_shares {
            return Ok(0);
        }
        let netted = debt
            .checked_sub(Debt::shares_to_debt(remaining, index)?)
            .ok_or(ErrorCode::BrokenInvariant)?;
        let protocol = checked_u64(mul_div_u128(netted, ledger.protocol_unpaid, ledger.interest()?)?)?;
        let side = self.side(asset);
        if netted == 0 || netted > owed || protocol > side.reserves.cash_reserve {
            return Ok(0);
        }
        match asset {
            MarketAsset::Base => self.quote_hlp_vault.debt_shares = remaining,
            MarketAsset::Quote => self.base_hlp_vault.debt_shares = remaining,
        }
        let side = self.side_mut(asset);
        side.debit_reserve(protocol, true)?;
        let ledger = &mut side.fees.ledger;
        ledger.funding_recognized -= netted;
        ledger.credited = ledger.credited.checked_sub(netted).ok_or(ErrorCode::BrokenInvariant)?;
        ledger.protocol_unpaid = ledger
            .protocol_unpaid
            .checked_sub(u128::from(protocol))
            .ok_or(ErrorCode::BrokenInvariant)?;
        ledger.protocol_pending = ledger
            .protocol_pending
            .checked_add(protocol)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        checked_u64(netted)
    }

    /// Bring both recognized interest sources up to the debt they measure,
    /// without netting: borrow rounding, share-burn phantom atoms and hLP
    /// plan drift. A decrease (an atom or two of share-burn drift) is written
    /// off like any other unpaid interest.
    pub(crate) fn recognize_interest(&mut self, asset: MarketAsset) -> Result<InterestCredit> {
        self.recognize(asset, false)
    }

    /// Recognize interest accrued by an index advance. Each hLP vault nets its
    /// new public credit against the funding it owes in the same asset.
    pub(crate) fn recognize_accrued_interest(&mut self, asset: MarketAsset) -> Result<InterestCredit> {
        self.recognize(asset, true)
    }

    /// Funding first, so public crediting nets against funding already
    /// credited. A decrease is a write-off; its pool loss is absorbed once
    /// both sources are recognized, so the loss valuation and the deficit
    /// test never see the curve inflated by interest accrued into the live
    /// reserve but not yet credited.
    #[inline(never)]
    fn recognize(&mut self, asset: MarketAsset, net: bool) -> Result<InterestCredit> {
        let mut receipt = InterestCredit::default();
        let mut lp_loss = None;
        let funding_now = self.funding_unpaid_interest(asset)?;
        let funding_recognized = self.side(asset).fees.ledger.funding_recognized;
        if funding_now > funding_recognized {
            receipt.add(self.credit_interest(
                asset,
                InterestSource::Funding,
                funding_now - funding_recognized,
                false,
            )?);
        } else if funding_now < funding_recognized {
            let settlement = self.settle_removed_interest(
                asset,
                InterestSource::Funding,
                funding_recognized - funding_now,
                0,
                ProtocolCash::Required,
            )?;
            lp_loss = Some(settlement.lp_loss);
        }
        let public_now = self.public_unpaid_interest(asset)?;
        let public_recognized = self.side(asset).fees.ledger.public_recognized;
        if public_now > public_recognized {
            receipt.add(self.credit_interest(asset, InterestSource::Public, public_now - public_recognized, net)?);
        } else if public_now < public_recognized {
            let settlement = self.settle_removed_interest(
                asset,
                InterestSource::Public,
                public_recognized - public_now,
                0,
                ProtocolCash::Required,
            )?;
            lp_loss = Some(
                lp_loss
                    .unwrap_or(0)
                    .checked_add(settlement.lp_loss)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
            );
        }
        if let Some(loss) = lp_loss {
            self.absorb_funded_credit_loss(asset, loss)?;
        }
        Ok(receipt)
    }

    /// Settle public interest removed by a path that already changed debt
    /// shares and principal. `paid` is the interest the borrower paid in
    /// cash; the rest of the removal is written off. A measured increase
    /// (a share-burn phantom atom) is credited instead.
    #[inline(never)]
    pub(crate) fn settle_public_interest(
        &mut self,
        asset: MarketAsset,
        paid: u64,
        protocol: ProtocolCash,
    ) -> Result<InterestSettlement> {
        let before = self.side(asset).fees.ledger.public_recognized;
        let now = self.public_unpaid_interest(asset)?;
        if now > before {
            require_eq!(paid, 0, ErrorCode::BrokenInvariant);
            self.credit_interest(asset, InterestSource::Public, now - before, false)?;
            return Ok(InterestSettlement::default());
        }
        self.settle_removed_interest(asset, InterestSource::Public, before - now, paid, protocol)
    }

    /// Settle hLP funding interest whose payment and cancellation the caller
    /// knows exactly. The caller absorbs `lp_loss` when `written_off > 0`.
    pub(crate) fn settle_funding_interest(
        &mut self,
        asset: MarketAsset,
        paid: u64,
        written_off: u64,
        protocol: ProtocolCash,
    ) -> Result<InterestSettlement> {
        let removed = u128::from(paid)
            .checked_add(u128::from(written_off))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.settle_removed_interest(asset, InterestSource::Funding, removed, paid, protocol)
    }

    #[inline(never)]
    fn settle_removed_interest(
        &mut self,
        asset: MarketAsset,
        source: InterestSource,
        removed: u128,
        paid: u64,
        protocol: ProtocolCash,
    ) -> Result<InterestSettlement> {
        if removed == 0 {
            require_eq!(paid, 0, ErrorCode::BrokenInvariant);
            return Ok(InterestSettlement::default());
        }
        let side = self.side_mut(asset);
        let (protocol_paid, protocol_written_off) = side.fees.ledger.settle(source, removed, paid)?;
        let written_off = removed - u128::from(paid);
        let protocol_paid = checked_u64(protocol_paid)?;
        let protocol_paid = match protocol {
            ProtocolCash::Required => protocol_paid,
            ProtocolCash::IfCashAllows => protocol_paid.min(side.reserves.cash_reserve),
        };
        side.debit_reserve(protocol_paid, true)?;
        side.fees.ledger.protocol_pending = side
            .fees
            .ledger
            .protocol_pending
            .checked_add(protocol_paid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let written_off = checked_u64(written_off)?;
        let protocol_written_off = checked_u64(protocol_written_off)?;
        Ok(InterestSettlement {
            protocol_paid,
            written_off,
            protocol_written_off,
            lp_loss: written_off - protocol_written_off,
        })
    }

    /// Reserve and ledger leg of a public debt reduction whose shares and
    /// principal the caller already removed. Cash receives `cash_repaid`;
    /// the debt not covered by it is a write-off absorbed junior first.
    /// Returns the protocol share paid and the absorbed pool loss.
    #[inline(never)]
    pub(crate) fn settle_public_debt(
        &mut self,
        asset: MarketAsset,
        cash_repaid: u64,
        interest_paid: u64,
        debt_removed: u64,
    ) -> Result<(u64, u64)> {
        let side = self.side_mut(asset);
        side.reserves.cash_reserve = side
            .reserves
            .cash_reserve
            .checked_add(cash_repaid)
            .ok_or(ErrorCode::ReserveOverflow)?;
        side.reserves.live_reserve = side
            .reserves
            .live_reserve
            .checked_add(u128::from(cash_repaid))
            .and_then(|value| value.checked_sub(u128::from(debt_removed)))
            .ok_or(ErrorCode::ReserveUnderflow)?;
        let settlement = self.settle_public_interest(asset, interest_paid, ProtocolCash::Required)?;
        let loss = debt_removed
            .checked_sub(cash_repaid)
            .and_then(|value| value.checked_sub(settlement.protocol_written_off))
            .ok_or(ErrorCode::BrokenInvariant)?;
        self.absorb_funded_credit_loss(asset, loss)?;
        Ok((settlement.protocol_paid, loss))
    }

    /// Credit liquidation coverage collected for junior. The atoms are in the
    /// reserve vault; they enter cash and the credited ledger together.
    pub(crate) fn record_junior_liquidation_credit(&mut self, asset: MarketAsset, credit: u64) -> Result<()> {
        if credit == 0 {
            return Ok(());
        }
        let side = self.side_mut(asset);
        require!(side.fees.junior.initialized, ErrorCode::BrokenInvariant);
        let senior = side
            .fees
            .junior
            .allocate(credit, TrancheRevenueSource::LiquidationCoverage)?;
        require_eq!(senior, 0, ErrorCode::BrokenInvariant);
        side.credit_reserve(credit, true)?;
        side.fees.ledger.credited = side
            .fees
            .ledger
            .credited
            .checked_add(u128::from(credit))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }

    /// `assert_interest_ledger` for both assets. Every instruction that
    /// mutates the ledger ends with it (`ledger_checked!` in `lib.rs`).
    pub fn assert_interest_ledgers(&self) -> Result<()> {
        self.assert_interest_ledger(MarketAsset::Base)?;
        self.assert_interest_ledger(MarketAsset::Quote)
    }

    /// Check the ledger against the debt it measures. Every instruction ends
    /// with both sources recognized.
    pub(crate) fn assert_interest_ledger(&self, asset: MarketAsset) -> Result<()> {
        let ledger = &self.side(asset).fees.ledger;
        require!(
            ledger.public_recognized == self.public_unpaid_interest(asset)?,
            ErrorCode::BrokenInvariant
        );
        require!(
            ledger.funding_recognized == self.funding_unpaid_interest(asset)?,
            ErrorCode::BrokenInvariant
        );
        require!(ledger.protocol_unpaid <= ledger.interest()?, ErrorCode::BrokenInvariant);
        require!(ledger.operator_claimable <= ledger.credited, ErrorCode::BrokenInvariant);
        require!(
            u128::from(self.side(asset).fees.junior.seed_interest_amount) <= ledger.credited,
            ErrorCode::BrokenInvariant
        );
        // Any loss beyond LP capital is recorded: C + D + I + Δ = P + Q + Pu.
        require!(
            self.side(asset).unrecorded_interest_deficit()? == 0,
            ErrorCode::BrokenInvariant
        );
        Ok(())
    }
}

impl Fees {
    /// Move the protocol interest paid since the last booking into the
    /// referral, fee and buyback liabilities. Its atoms already sit in the
    /// reserve vault outside cash.
    pub(crate) fn book_protocol_interest(&mut self, split: ProtocolAuctionSplit, referral: u64) -> Result<()> {
        let pending = self.ledger.protocol_pending;
        require_gte!(pending, referral, ErrorCode::BrokenInvariant);
        if pending == 0 {
            return Ok(());
        }
        self.credit_protocol_interest_revenue(pending, split, referral)?;
        self.ledger.protocol_pending = 0;
        Ok(())
    }
}

impl MarketSide {
    /// Pay credited interest from reserve cash, limited by the cash present.
    /// Returns the amount paid; the rest stays claimable.
    pub(crate) fn pay_interest(&mut self, amount: u64) -> Result<u64> {
        let paid = amount.min(self.reserves.cash_reserve);
        if paid == 0 {
            return Ok(0);
        }
        self.fees.ledger.credited = self
            .fees
            .ledger
            .credited
            .checked_sub(u128::from(paid))
            .ok_or(ErrorCode::UnbackedFeeLiability)?;
        self.debit_reserve(paid, true)?;
        Ok(paid)
    }
}

impl Market {
    /// Book both assets' pending protocol interest with no referral. Never
    /// call it after a borrower payment and before that payment's referral is
    /// booked (`book_paid_protocol_interest`): it would empty the pending
    /// share the referral is carved from.
    pub(crate) fn book_protocol_interest(&mut self, split: ProtocolAuctionSplit) -> Result<()> {
        self.base_side.fees.book_protocol_interest(split, 0)?;
        self.quote_side.fees.book_protocol_interest(split, 0)
    }

    /// Book everything pending after a borrower's interest payment: the debt
    /// asset carves `referral` from its pending share (which holds the
    /// payment's protocol share, plus any hLP funding share paid in the same
    /// instruction), the other asset books without a referral.
    pub(crate) fn book_paid_protocol_interest(
        &mut self,
        debt_asset: MarketAsset,
        split: ProtocolAuctionSplit,
        referral: u64,
    ) -> Result<()> {
        self.side_mut(debt_asset).fees.book_protocol_interest(split, referral)?;
        self.side_mut(debt_asset.opposite())
            .fees
            .book_protocol_interest(split, 0)
    }
}

#[cfg(test)]
mod tests {
    include!("../tests/transitions/interest_ledger.rs");
}
