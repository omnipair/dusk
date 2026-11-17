use super::*;
use crate::{
    constants::{MIN_LIQUIDITY, YIELD_GROWTH_SCALE_Q64},
    math::accrue_fee_liability_with_remainder,
    state::{BorrowPosition, LiquidationLossAllocation},
};
use proptest::prelude::*;

mod operator_revenue {
    include!("operator_revenue.rs");
}

mod ledger_backstops {
    include!("ledger_backstops.rs");
}

mod deficit_recheck {
    include!("deficit_recheck.rs");
}

const ASSETS: [MarketAsset; 2] = [MarketAsset::Base, MarketAsset::Quote];
const LIQUIDITY: u64 = 10_000_000;

/// One holder of an index lane: a balance checkpointed against the lane's
/// growth index, with its own sub-atom carry, and the exact entitlement
/// `Σ balance × Δindex` (scaled by 2^64) over its holding periods.
#[derive(Clone, Copy, Debug, Default)]
struct Holder {
    balance: u64,
    checkpoint: [u128; 2],
    remainder: [u64; 2],
    accrued: [u64; 2],
    paid: [u64; 2],
    exact_scaled: [u128; 2],
}

impl Holder {
    fn checkpoint(&mut self, index: [u128; 2]) {
        for (i, &now) in index.iter().enumerate() {
            let (amount, remainder) =
                accrue_fee_liability_with_remainder(self.balance, now, self.checkpoint[i], self.remainder[i]).unwrap();
            self.exact_scaled[i] += u128::from(self.balance) * (now - self.checkpoint[i]);
            self.accrued[i] += amount;
            self.remainder[i] = remainder;
            self.checkpoint[i] = now;
        }
    }

    /// Entitlement of record without mutating: accrued plus pending.
    fn entitlement(&self, index: [u128; 2], asset: usize) -> u64 {
        let (pending, _) = accrue_fee_liability_with_remainder(
            self.balance,
            index[asset],
            self.checkpoint[asset],
            self.remainder[asset],
        )
        .unwrap();
        self.accrued[asset] + pending
    }
}

#[derive(Clone, Debug)]
enum Op {
    Accrue { asset: usize, bps: u32 },
    Borrow { asset: usize, position: usize, amount: u64 },
    Repay { asset: usize, position: usize, bps: u16 },
    Default { asset: usize, position: usize, recovery_bps: u16 },
    PayFunding { asset: usize, bps: u16 },
    Claim { holder: usize, asset: usize },
    ClaimVault { vault: usize, asset: usize },
    ClaimOperator { asset: usize, max: u64 },
    Transfer { from: usize, to: usize, bps: u16 },
    Book,
    Freeze { vault: usize },
    Thaw { vault: usize },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..2usize, 1..2_000u32).prop_map(|(asset, bps)| Op::Accrue { asset, bps }),
        // Rare large jumps let credited interest outgrow LP capital, so a
        // later default reaches the deficit.
        2 => (0..2usize, 10_000..100_000u32).prop_map(|(asset, bps)| Op::Accrue { asset, bps }),
        3 => (0..2usize, 0..3usize, 1..2_000_000u64).prop_map(|(asset, position, amount)| Op::Borrow { asset, position, amount }),
        3 => (0..2usize, 0..3usize, 1..=10_000u16).prop_map(|(asset, position, bps)| Op::Repay { asset, position, bps }),
        2 => (0..2usize, 0..3usize, 0..=12_000u16).prop_map(|(asset, position, recovery_bps)| Op::Default { asset, position, recovery_bps }),
        2 => (0..2usize, 1..=10_000u16).prop_map(|(asset, bps)| Op::PayFunding { asset, bps }),
        3 => (1..4usize, 0..2usize).prop_map(|(holder, asset)| Op::Claim { holder, asset }),
        1 => (0..2usize, 0..2usize).prop_map(|(vault, asset)| Op::ClaimVault { vault, asset }),
        1 => (0..2usize, 1..u64::MAX).prop_map(|(asset, max)| Op::ClaimOperator { asset, max }),
        2 => (1..4usize, 1..4usize, 1..=10_000u16).prop_map(|(from, to, bps)| Op::Transfer { from, to, bps }),
        1 => Just(Op::Book),
        1 => (0..2usize).prop_map(|vault| Op::Freeze { vault }),
        1 => (0..2usize).prop_map(|vault| Op::Thaw { vault }),
    ]
}

/// Ordinary srLP holders (index 0 is the permanently burned minimum), one
/// holder per hLP vault, the operator, and every public position.
struct Model {
    market: Market,
    holders: [Holder; 4],
    vaults: [Holder; 2],
    positions: [[BorrowPosition; 3]; 2],
    protocol_accrued: [u128; 2],
    protocol_booked: [u128; 2],
    credited_in: [u128; 2],
    paid_out: [u128; 2],
}

fn asset(code: usize) -> MarketAsset {
    ASSETS[code]
}

impl Model {
    fn new(markup: u16, operator_share: u16, vault_shares: [u64; 2], funding: [u64; 2]) -> Self {
        let mut market = Market::current_layout();
        market.debt.base_borrow_index_nad = NAD as u128;
        market.debt.quote_borrow_index_nad = NAD as u128;
        market.protocol_interest_markup_bps = markup;
        market.operator.interest_share_bps = operator_share;
        for side in [&mut market.base_side, &mut market.quote_side] {
            side.shares.srlp_supply = LIQUIDITY;
            side.reserves.live_reserve = u128::from(LIQUIDITY);
            side.reserves.cash_reserve = LIQUIDITY;
        }
        market.base_hlp_vault.srlp_shares = vault_shares[0];
        market.base_hlp_vault.hlp_supply = vault_shares[0];
        market.quote_hlp_vault.srlp_shares = vault_shares[1];
        market.quote_hlp_vault.hlp_supply = vault_shares[1];
        // The base vault borrows quote and the quote vault borrows base.
        market.base_hlp_vault.debt_shares = u128::from(funding[1]);
        market.base_hlp_vault.debt_principal = funding[1];
        market.base_hlp_vault.quote_hlp_live_reserve = funding[1];
        market.quote_side.reserves.cash_reserve -= funding[1];
        market.quote_hlp_vault.debt_shares = u128::from(funding[0]);
        market.quote_hlp_vault.debt_principal = funding[0];
        market.quote_hlp_vault.base_hlp_live_reserve = funding[0];
        market.base_side.reserves.cash_reserve -= funding[0];
        let ordinary = LIQUIDITY - MIN_LIQUIDITY - vault_shares[0] - vault_shares[1];
        let mut holders = [Holder::default(); 4];
        holders[0].balance = MIN_LIQUIDITY;
        holders[1].balance = ordinary / 2;
        holders[2].balance = ordinary / 3;
        holders[3].balance = ordinary - ordinary / 2 - ordinary / 3;
        let mut vaults = [Holder::default(); 2];
        vaults[0].balance = vault_shares[0];
        vaults[1].balance = vault_shares[1];
        let position = BorrowPosition {
            auction_debt_asset: u8::MAX,
            ..BorrowPosition::default()
        };
        Self {
            market,
            holders,
            vaults,
            positions: std::array::from_fn(|_| std::array::from_fn(|_| position.clone())),
            protocol_accrued: [0; 2],
            protocol_booked: [0; 2],
            credited_in: [0; 2],
            paid_out: [0; 2],
        }
    }

    fn srlp_index(&self) -> [u128; 2] {
        [
            self.market.base_side.fees.interest_growth_index_q64,
            self.market.quote_side.fees.interest_growth_index_q64,
        ]
    }

    fn vault_index(&self, vault: usize) -> [u128; 2] {
        let vault = if vault == 0 { &self.market.base_hlp_vault } else { &self.market.quote_hlp_vault };
        [vault.base_interest_growth_index_q64, vault.quote_interest_growth_index_q64]
    }

    fn curve(&self, code: usize) -> u64 {
        self.market.curve_reserve(asset(code)).unwrap()
    }

    /// Credits net of netting, which removes the borrowing vault's credit
    /// from `credited` together with its funding debt.
    fn record_credit(&mut self, code: usize, credited_before: u128, credit: InterestCredit) {
        self.protocol_accrued[code] += u128::from(credit.protocol);
        self.credited_in[code] += self.market.side(asset(code)).fees.ledger.credited - credited_before;
    }

    fn public_debt(&self, code: usize) -> u128 {
        match asset(code) {
            MarketAsset::Base => self.market.debt.fixed_base_debt().unwrap(),
            MarketAsset::Quote => self.market.debt.fixed_quote_debt().unwrap(),
        }
    }

    fn apply(&mut self, op: &Op) -> std::result::Result<(), TestCaseError> {
        match *op {
            Op::Accrue { asset: code, bps } => {
                let a = asset(code);
                let before = self.public_debt(code);
                let curve = self.curve(code);
                let credited = self.market.side(a).fees.ledger.credited;
                let index = self.market.debt.borrow_index(a);
                let next = index + index * u128::from(bps) / 10_000;
                if next > 10_000 * NAD as u128 {
                    return Ok(());
                }
                match a {
                    MarketAsset::Base => self.market.debt.base_borrow_index_nad = next,
                    MarketAsset::Quote => self.market.debt.quote_borrow_index_nad = next,
                }
                let accrued = self.public_debt(code) - before;
                self.market.side_mut(a).reserves.live_reserve += accrued;
                let credit = self.market.recognize_accrued_interest(a).unwrap();
                self.record_credit(code, credited, credit);
                // Property 2: accrual and netting never move the curve.
                prop_assert_eq!(self.curve(code), curve);
            }
            Op::Borrow { asset: code, position, amount } => {
                let a = asset(code);
                let cash = self.market.side(a).reserves.cash_reserve;
                if amount >= cash {
                    return Ok(());
                }
                let index = self.market.debt.borrow_index(a);
                let shares = Debt::debt_to_shares(amount, index).unwrap();
                let before = self.public_debt(code);
                let p = &mut self.positions[code][position];
                match a {
                    MarketAsset::Base => {
                        p.fixed_base_shares += shares;
                        p.fixed_base_principal += amount;
                        self.market.debt.fixed_base_shares += shares;
                        self.market.debt.fixed_base_principal += amount;
                    }
                    MarketAsset::Quote => {
                        p.fixed_quote_shares += shares;
                        p.fixed_quote_principal += amount;
                        self.market.debt.fixed_quote_shares += shares;
                        self.market.debt.fixed_quote_principal += amount;
                    }
                }
                let increase = self.public_debt(code) - before;
                let side = self.market.side_mut(a);
                side.reserves.cash_reserve -= amount;
                side.reserves.live_reserve = side.reserves.live_reserve + increase - u128::from(amount);
                let credited = self.market.side(a).fees.ledger.credited;
                let credit = self.market.recognize_interest(a).unwrap();
                self.record_credit(code, credited, credit);
            }
            Op::Repay { asset: code, position, bps } => {
                let a = asset(code);
                let mut p = self.positions[code][position].clone();
                let debt = match a {
                    MarketAsset::Base => p.fixed_base_debt(&self.market.debt).unwrap(),
                    MarketAsset::Quote => p.fixed_quote_debt(&self.market.debt).unwrap(),
                };
                if debt == 0 {
                    return Ok(());
                }
                let max = u64::try_from((debt * u128::from(bps) / 10_000).max(1)).unwrap();
                let Ok(quote) = self.market.fixed_repayment_for_max(&p, a, max) else {
                    return Ok(());
                };
                let curve = self.curve(code);
                let ledger_before = self.market.side(a).fees.ledger;
                let aggregate = self.market.debt.fixed_debt_reduction_for_shares(a, quote.shares_to_burn).unwrap();
                let (interest_paid, _) = self
                    .market
                    .clear_fixed_position_principal(&mut p, a, quote.cash_repaid, quote.shares_to_burn, aggregate)
                    .unwrap();
                match a {
                    MarketAsset::Base => {
                        p.fixed_base_shares -= quote.shares_to_burn;
                        self.market.debt.fixed_base_shares -= quote.shares_to_burn;
                    }
                    MarketAsset::Quote => {
                        p.fixed_quote_shares -= quote.shares_to_burn;
                        self.market.debt.fixed_quote_shares -= quote.shares_to_burn;
                    }
                }
                self.positions[code][position] = p;
                let (protocol_paid, loss) = self
                    .market
                    .settle_public_debt(a, quote.cash_repaid, interest_paid, aggregate)
                    .unwrap();
                // A share burn can move a phantom atom from principal to
                // interest; it is recognized like any accrued atom.
                let ledger = self.market.side(a).fees.ledger;
                let credited = ledger.credited - ledger_before.credited;
                let protocol = (ledger.protocol_unpaid + u128::from(protocol_paid)).saturating_sub(ledger_before.protocol_unpaid);
                self.credited_in[code] += credited;
                self.protocol_accrued[code] += protocol;
                prop_assert!(credited + protocol <= 2);
                // Property 2: a full-cash repayment leaves the curve unchanged
                // up to those phantom atoms.
                if loss == 0 {
                    prop_assert!(self.curve(code).abs_diff(curve) <= 2);
                }
            }
            Op::Default { asset: code, position, recovery_bps } => {
                let a = asset(code);
                let mut p = self.positions[code][position].clone();
                let (shares, principal) = match a {
                    MarketAsset::Base => (p.fixed_base_shares, p.fixed_base_principal),
                    MarketAsset::Quote => (p.fixed_quote_shares, p.fixed_quote_principal),
                };
                if shares == 0 {
                    return Ok(());
                }
                // Settlement measures the aggregate debt the burned shares
                // remove, as the liquidation paths do.
                let removed = self.market.debt.fixed_debt_reduction_for_shares(a, shares).unwrap();
                let recovery = (u128::from(removed) * u128::from(recovery_bps) / 10_000).min(u128::from(removed)) as u64;
                let loss = LiquidationLossAllocation::quote(principal.min(removed), removed, recovery).unwrap();
                // Property 7: a short recovery pays principal first.
                if recovery < principal {
                    prop_assert_eq!(loss.interest_paid, 0);
                }
                let ledger_before = self.market.side(a).fees.ledger;
                let curve = self.curve(code);
                match a {
                    MarketAsset::Base => {
                        self.market.debt.fixed_base_shares -= shares;
                        self.market.debt.fixed_base_principal -= principal;
                        p.fixed_base_shares = 0;
                        p.fixed_base_principal = 0;
                    }
                    MarketAsset::Quote => {
                        self.market.debt.fixed_quote_shares -= shares;
                        self.market.debt.fixed_quote_principal -= principal;
                        p.fixed_quote_shares = 0;
                        p.fixed_quote_principal = 0;
                    }
                }
                self.positions[code][position] = p;
                let (protocol_paid, pool_loss) =
                    self.market.settle_public_debt(a, recovery, loss.interest_paid, removed).unwrap();
                let ledger = self.market.side(a).fees.ledger;
                if loss.interest_paid == 0 {
                    prop_assert_eq!(protocol_paid, 0);
                }
                // Property 5: credited interest and the operator's share never
                // absorb the default; the curve takes the pool loss.
                prop_assert_eq!(ledger.credited, ledger_before.credited);
                prop_assert_eq!(ledger.operator_claimable, ledger_before.operator_claimable);
                prop_assert!(ledger.protocol_unpaid <= ledger_before.protocol_unpaid);
                // LP capital absorbs the pool loss down to zero; the rest is
                // the recorded deficit, and credited claims stay whole.
                prop_assert_eq!(self.curve(code), curve.saturating_sub(pool_loss));
                prop_assert_eq!(
                    ledger.deficit - ledger_before.deficit,
                    u128::from(pool_loss.saturating_sub(curve))
                );
            }
            Op::PayFunding { asset: code, bps } => {
                let a = asset(code);
                let interest = self.market.funding_unpaid_interest(a).unwrap();
                if interest == 0 {
                    return Ok(());
                }
                let index = self.market.debt.borrow_index(a);
                let (shares, principal) = {
                    let vault = self.market.funding_vault(a);
                    (vault.debt_shares, vault.debt_principal)
                };
                let debt = Debt::shares_to_debt(shares, index).unwrap();
                let pay = (interest * u128::from(bps) / 10_000).max(1);
                let target = (debt - pay).max(u128::from(principal));
                let remaining = crate::math::mul_div_ceil_u128(target, NAD as u128, index).unwrap();
                if remaining >= shares {
                    return Ok(());
                }
                let paid = u64::try_from(debt - Debt::shares_to_debt(remaining, index).unwrap()).unwrap();
                let protocol = self.market.side(a).fees.ledger.protocol_share(paid).unwrap();
                if protocol > self.market.side(a).reserves.cash_reserve || paid > self.curve(code) {
                    return Ok(());
                }
                let curve = self.curve(code);
                let cash = self.market.side(a).reserves.cash_reserve;
                match a {
                    MarketAsset::Base => self.market.quote_hlp_vault.debt_shares = remaining,
                    MarketAsset::Quote => self.market.base_hlp_vault.debt_shares = remaining,
                }
                let settlement = self
                    .market
                    .settle_funding_interest(a, paid, 0, ProtocolCash::Required)
                    .unwrap();
                // Property 2: a funding payment changes the curve by exactly
                // `−paid` and cash only by its protocol share.
                prop_assert_eq!(self.curve(code), curve - paid);
                prop_assert_eq!(self.market.side(a).reserves.cash_reserve, cash - settlement.protocol_paid);
            }
            Op::Claim { holder, asset: code } => {
                let index = self.srlp_index();
                let curve = self.curve(code);
                let h = &mut self.holders[holder];
                h.checkpoint(index);
                let paid = self.market.side_mut(asset(code)).pay_interest(h.accrued[code]).unwrap();
                h.accrued[code] -= paid;
                h.paid[code] += paid;
                self.paid_out[code] += u128::from(paid);
                prop_assert_eq!(self.curve(code), curve);
            }
            Op::ClaimVault { vault, asset: code } => {
                let index = self.vault_index(vault);
                let h = &mut self.vaults[vault];
                h.checkpoint(index);
                let paid = self.market.side_mut(asset(code)).pay_interest(h.accrued[code]).unwrap();
                h.accrued[code] -= paid;
                h.paid[code] += paid;
                self.paid_out[code] += u128::from(paid);
            }
            Op::ClaimOperator { asset: code, max } => {
                let a = asset(code);
                let curve = self.curve(code);
                let request = self.market.side(a).fees.ledger.operator_claimable.min(u128::from(max)) as u64;
                let paid = self.market.side_mut(a).pay_interest(request).unwrap();
                self.market.side_mut(a).fees.ledger.operator_claimable -= u128::from(paid);
                self.paid_out[code] += u128::from(paid);
                prop_assert_eq!(self.curve(code), curve);
            }
            Op::Transfer { from, to, bps } => {
                if from == to {
                    return Ok(());
                }
                let index = self.srlp_index();
                self.holders[from].checkpoint(index);
                self.holders[to].checkpoint(index);
                let amount = self.holders[from].balance * u64::from(bps) / 10_000;
                self.holders[from].balance -= amount;
                self.holders[to].balance += amount;
            }
            Op::Book => {
                for code in 0..2 {
                    let curve = self.curve(code);
                    self.protocol_booked[code] += u128::from(self.market.side(asset(code)).fees.ledger.protocol_pending);
                    prop_assert_eq!(self.curve(code), curve);
                }
                self.market.book_protocol_interest(ProtocolAuctionSplit::default()).unwrap();
            }
            Op::Freeze { vault } => {
                // A frozen vault's later interest is held for its creditors.
                self.market.amm.frozen_hlp_mask |= 1 << vault;
            }
            Op::Thaw { vault } => {
                // Unfreezing publishes the held interest to the holders
                // without moving `credited` or the curve.
                let target = asset(vault);
                if self.market.hlp_frozen(target) {
                    let before = [0, 1].map(|code| (self.curve(code), self.market.side(asset(code)).fees.ledger.credited));
                    self.market.clear_frozen_hlp(target).unwrap();
                    let v = if vault == 0 { &self.market.base_hlp_vault } else { &self.market.quote_hlp_vault };
                    prop_assert_eq!(v.held_interest(target), (0, 0));
                    for code in 0..2 {
                        prop_assert_eq!((self.curve(code), self.market.side(asset(code)).fees.ledger.credited), before[code]);
                    }
                }
            }
        }
        Ok(())
    }

    fn check(&self) -> std::result::Result<(), TestCaseError> {
        let srlp_index = self.srlp_index();
        for code in 0..2 {
            let a = asset(code);
            // Property 1: recognized equals computed, `Pu ≤ I`, the curve is
            // non-negative, and the live identity holds.
            self.market.assert_interest_ledger(a).unwrap();
            self.market.assert_virtual_reserve_invariant(a).unwrap();
            let ledger = self.market.side(a).fees.ledger;
            // C + D + I + Δ = P + Q + Pu, with the virtual reserve invariant
            // above giving `live = C + D + I_public + hLP backing`.
            prop_assert_eq!(
                self.market.side(a).reserves.live_reserve + ledger.funding_recognized + ledger.deficit,
                u128::from(self.curve(code)) + ledger.credited + ledger.protocol_unpaid
            );
            let interest = ledger.public_recognized + ledger.funding_recognized;
            // Property 6: the protocol share is paid only, never above I.
            prop_assert!(ledger.protocol_unpaid <= interest);
            if interest == 0 {
                prop_assert_eq!(ledger.protocol_unpaid, 0);
            }
            let protocol_paid = self.protocol_booked[code] + u128::from(ledger.protocol_pending);
            prop_assert!(protocol_paid + ledger.protocol_unpaid <= self.protocol_accrued[code]);
            // Property 3: payouts never exceed credits, and everything
            // credited is owed to a holder of record up to index dust.
            prop_assert!(self.paid_out[code] <= self.credited_in[code]);
            prop_assert_eq!(self.credited_in[code] - self.paid_out[code], ledger.credited);
            let mut owed = ledger.operator_claimable;
            for holder in &self.holders {
                owed += u128::from(holder.entitlement(srlp_index, code));
            }
            for vault in 0..2 {
                owed += u128::from(self.vaults[vault].entitlement(self.vault_index(vault), code));
            }
            let hlp = [&self.market.base_hlp_vault, &self.market.quote_hlp_vault];
            for vault in hlp {
                owed += u128::from(match a {
                    MarketAsset::Base => vault.unallocated_base_interest_amount + vault.held_base_interest,
                    MarketAsset::Quote => vault.unallocated_quote_interest_amount + vault.held_quote_interest,
                });
            }
            prop_assert!(owed <= ledger.credited, "owed {} above credited {}", owed, ledger.credited);
            // Sub-atom carries: the ordinary lane, two vault lanes, and one
            // per holder checkpoint (four srLP holders, two hLP holders).
            let dust = 9;
            prop_assert!(
                ledger.credited - owed <= dust,
                "credited {} exceeds owed {} by more than {} dust atoms",
                ledger.credited,
                owed,
                dust
            );
            // Property 4: each holder's paid plus accrued interest is the
            // floor of its exact entitlement over its holding periods.
            for holder in &self.holders {
                let mut h = *holder;
                h.checkpoint(srlp_index);
                let exact = h.exact_scaled[code] / YIELD_GROWTH_SCALE_Q64;
                prop_assert!(u128::from(h.accrued[code] + h.paid[code]) <= exact);
                prop_assert!(exact - u128::from(h.accrued[code] + h.paid[code]) <= 1);
            }
        }
        Ok(())
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// Random accrual, borrow, repayment, default, funding payment, claim,
    /// transfer and booking sequences against the ledger kernels, checking
    /// properties 1-4, 6 and 7 after every step and the curve rules of
    /// property 2 inside each step.
    #[test]
    fn ledger_sequences_conserve_credit_and_curve(
        markup in prop_oneof![Just(0u16), Just(500), 1..=10_000u16],
        operator_share in prop_oneof![Just(0u16), Just(3_000), 0..=10_000u16],
        base_vault in 0..2_000_000u64,
        quote_vault in 0..2_000_000u64,
        base_funding in 0..1_000_000u64,
        quote_funding in 0..1_000_000u64,
        ops in proptest::collection::vec(op(), 1..48),
    ) {
        let mut model = Model::new(
            markup,
            operator_share,
            [base_vault, quote_vault],
            [if quote_vault > 0 { base_funding } else { 0 }, if base_vault > 0 { quote_funding } else { 0 }],
        );
        model.check()?;
        for op in &ops {
            model.apply(op)?;
            model.check()?;
        }
    }

    /// Property 5: a default reduces junior capital before senior by exactly
    /// the LP-and-operator part written off; credited interest, the
    /// operator's share and the protocol's paid share are untouched, and the
    /// protocol's unpaid share falls by its written-off part.
    #[test]
    fn default_is_absorbed_by_junior_before_senior(
        markup in 0..=10_000u16,
        operator_share in 0..=10_000u16,
        interest_bps in 1..5_000u128,
        written_off_bps in 1..=10_000u128,
    ) {
        let mut market = Market::current_layout();
        market.debt.base_borrow_index_nad = NAD as u128;
        market.debt.quote_borrow_index_nad = NAD as u128;
        market.amm.launch_reference_price_nad = NAD;
        market.protocol_interest_markup_bps = markup;
        market.operator.interest_share_bps = operator_share;
        market.add_liquidity(100_000, 100_000).unwrap();
        let extra = 20_000 - market.tranches.junior_reserve_shares;
        market.tranches.enter(extra, 100_000).unwrap();
        market.synchronize_tranche_income();
        market.base_side.reserves.cash_reserve = 20_000;
        market.debt.fixed_base_shares = 80_000;
        market.debt.fixed_base_principal = 80_000;
        market.debt.base_borrow_index_nad = NAD as u128 * (10_000 + interest_bps) / 10_000;
        let interest = market.debt.fixed_base_debt().unwrap() - 80_000;
        market.base_side.reserves.live_reserve += interest;
        market.recognize_accrued_interest(MarketAsset::Base).unwrap();
        let removed_shares = 80_000 * written_off_bps / 10_000;
        if removed_shares == 0 {
            return Ok(());
        }
        let removed = market.debt.fixed_debt_reduction_for_shares(MarketAsset::Base, removed_shares).unwrap();
        let principal = (80_000 * written_off_bps / 10_000) as u64;
        let curve_before = market.curve_reserve(MarketAsset::Base).unwrap();
        let ledger_before = market.base_side.fees.ledger;
        let supply = market.base_side.shares.srlp_supply;
        let funded = market.funded_reserve_value().unwrap();
        let junior_before = market.tranches.junior_value(funded, supply).unwrap();
        let senior_shares = market.tranches.senior_reserve_shares(supply).unwrap();
        let senior_before = funded - junior_before;
        market.debt.fixed_base_shares -= removed_shares;
        market.debt.fixed_base_principal -= principal;
        let (protocol_paid, loss) = market.settle_public_debt(MarketAsset::Base, 0, 0, removed).unwrap();
        let ledger = market.base_side.fees.ledger;
        prop_assert_eq!(protocol_paid, 0);
        prop_assert_eq!(ledger.credited, ledger_before.credited);
        prop_assert_eq!(ledger.operator_claimable, ledger_before.operator_claimable);
        let protocol_written_off = ledger_before.protocol_unpaid - ledger.protocol_unpaid;
        prop_assert_eq!(u128::from(loss) + protocol_written_off, u128::from(removed));
        // A loss beyond LP capital zeroes it; the rest is the deficit.
        prop_assert_eq!(ledger.deficit, u128::from(loss.saturating_sub(curve_before)));
        if ledger.deficit > 0 {
            prop_assert_eq!(market.curve_reserve(MarketAsset::Base).unwrap(), 0);
            prop_assert_eq!(market.tranches.junior_reserve_shares, 0);
        }
        market.assert_interest_ledger(MarketAsset::Base).unwrap();
        let supply_after = market.base_side.shares.srlp_supply;
        let funded_after = market.funded_reserve_value().unwrap();
        let loss_value = funded - funded_after;
        let junior_after = market.tranches.junior_value(funded_after, supply_after).unwrap();
        let senior_after = funded_after - junior_after;
        prop_assert_eq!(market.tranches.senior_reserve_shares(supply_after).unwrap(), senior_shares);
        // One reserve share of value is the rounding of the junior burn,
        // which favours senior.
        let share_value = funded / u128::from(supply) + 1;
        if loss_value <= junior_before {
            prop_assert!(senior_after + share_value >= senior_before);
            prop_assert!(junior_after + loss_value + share_value >= junior_before);
            prop_assert!(junior_after + loss_value <= junior_before + share_value);
        } else {
            prop_assert_eq!(market.tranches.junior_reserve_shares, 0);
            let senior_loss = loss_value - junior_before;
            prop_assert!(senior_after.abs_diff(senior_before - senior_loss) <= share_value);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Property 9: with cash for its output, a spot swap never fails in
    /// recognition, netting or funding settlement, at any protocol markup
    /// and however little cash is left to pay the funding.
    #[test]
    fn spot_swap_never_fails_on_interest_accounting(
        markup in prop_oneof![Just(0u16), Just(2_000), 0..=10_000u16],
        funding_bps in 0..3_000u128,
        drained_bps in 0..=10_000u64,
        amplification in prop_oneof![Just(1u64), Just(4)],
        base_in in any::<bool>(),
        amount in 1_000_000u64..2_000_000_000,
    ) {
        use crate::state::AmmState;
        use crate::transitions::{amm::SwapRequest, liquidity::SwapCashPolicy, LeverageSwapFeeCredit};
        const UNIT: u64 = 1_000_000;
        let mut market = Market::current_layout();
        market.base_side.asset_decimals = 6;
        market.quote_side.asset_decimals = 6;
        market.protocol_interest_markup_bps = markup;
        market.debt.base_borrow_index_nad = NAD as u128;
        market.debt.quote_borrow_index_nad = NAD as u128;
        market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
        market.deposit_single_sided(MarketAsset::Base, 20_000 * UNIT, 1).unwrap();
        market.deposit_single_sided(MarketAsset::Quote, 20_000 * UNIT, 1).unwrap();
        market.config.amm.peak_amplification_nad = amplification * NAD;
        market.config.amm.core_half_width_bps = if amplification == 1 { 0 } else { 100 };
        market.config.amm.fade_width_bps = if amplification == 1 { 0 } else { 400 };
        market.config.swap_fee_bps = 30;
        *market.amm = AmmState::default();
        market.prepare_amm_for_swap(1).unwrap();
        market.risk.base_price_ema_nad = NAD;
        market.risk.quote_price_ema_nad = NAD;
        market.observe_current_risk(1).unwrap();
        let asset_in = if base_in { MarketAsset::Base } else { MarketAsset::Quote };
        let out_asset = asset_in.opposite();
        // Public borrowers drain the output asset's cash, then interest and
        // funding accrue at the same index.
        let cash = market.side(out_asset).reserves.cash_reserve;
        let borrowed = cash / 10_000 * drained_bps;
        if borrowed > 0 {
            let shares = Debt::debt_to_shares(borrowed, NAD as u128).unwrap();
            match out_asset {
                MarketAsset::Base => {
                    market.debt.fixed_base_shares = shares;
                    market.debt.fixed_base_principal = borrowed;
                }
                MarketAsset::Quote => {
                    market.debt.fixed_quote_shares = shares;
                    market.debt.fixed_quote_principal = borrowed;
                }
            }
            market.side_mut(out_asset).reserves.cash_reserve -= borrowed;
        }
        let before = market.public_unpaid_interest(out_asset).unwrap();
        match out_asset {
            MarketAsset::Base => market.debt.base_borrow_index_nad += u128::from(NAD) * funding_bps / 10_000,
            MarketAsset::Quote => market.debt.quote_borrow_index_nad += u128::from(NAD) * funding_bps / 10_000,
        }
        let accrued = market.public_unpaid_interest(out_asset).unwrap() - before;
        market.side_mut(out_asset).reserves.live_reserve += accrued;
        market.recognize_accrued_interest(out_asset).unwrap();
        market.recognize_interest(asset_in).unwrap();
        let request = SwapRequest {
            current_slot: 1,
            current_unix_timestamp: 0,
            asset_in,
            reserve_credit: amount,
            protocol_fee_bps: 0,
        };
        let Ok(mut prepared) = request.prepare(&mut market) else {
            // Refused only for its own output cash or the curve, never for
            // interest accounting: the same swap on a market whose funding
            // was already settled must refuse as well.
            return Ok(());
        };
        prop_assume!(market.side(out_asset).reserves.cash_reserve > prepared.quote.gross_amount_out);
        let quote = prepared.leverage_quote();
        let credit = LeverageSwapFeeCredit::from_total_actual_credit(&quote, prepared.quote.fee.claimable_fee_debit).unwrap();
        let result = prepared.apply(&mut market, SwapCashPolicy::Spot, credit, 1, 0, ProtocolAuctionSplit::default(), None);
        prop_assert!(result.is_ok(), "{:?}", result.err());
        market.assert_market_invariants().unwrap();
    }
}

#[test]
fn split_carries_conserve_every_accrued_atom() {
    let mut fractions = [0_u64; 2];
    let (mut protocol, mut operator) = (0_u64, 0_u64);
    let mut gross = 0_u64;
    for chunk in [1_u64, 2, 3, 7, 11, 13, 997, 1_000_003] {
        let p = split_with_fraction(chunk, 1_234, 11_234, &mut fractions[0]).unwrap();
        let o = split_with_fraction(chunk - p, 3_000, 10_000, &mut fractions[1]).unwrap();
        protocol += p;
        operator += o;
        gross += chunk;
    }
    // The carried fractions keep each running total within one atom below
    // the exact share of everything accrued so far.
    assert!(u128::from(protocol) <= u128::from(gross) * 1_234 / 11_234);
    assert!(u128::from(protocol) + 1 >= u128::from(gross) * 1_234 / 11_234);
    assert!(operator <= (gross - protocol) * 3 / 10 + 1);
    assert!(operator + 1 >= (gross - protocol) * 3 / 10);
}

