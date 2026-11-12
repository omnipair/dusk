use anchor_lang::prelude::*;

use crate::state::market::{Debt, MarketAsset};
use crate::{
    constants::{BPS_DENOMINATOR, DEFAULT_PROTOCOL_REVENUE_SHARE_BPS},
    errors::ErrorCode,
};

pub const LIQUIDATION_MINIMUM_DISCOUNT_BPS: u16 = 50;
pub const LIQUIDATION_RECOVERY_BUFFER_BPS: u16 = 200;
pub use crate::constants::LIQUIDATION_FEE_BPS;
pub const LIQUIDATION_PROTOCOL_SHARE_BPS: u16 = DEFAULT_PROTOCOL_REVENUE_SHARE_BPS;

/// Linear symmetric-EMA collateral value and actual indexed debt, both in debt
/// token atoms. Execution prices never determine reference eligibility.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct LiquidationHealth {
    pub collateral_value: u64,
    pub debt: u64,
    pub maintenance_bps: u16,
}

impl LiquidationHealth {
    pub fn validate(&self) -> Result<()> {
        require!(
            self.maintenance_bps > 0 && self.maintenance_bps < BPS_DENOMINATOR,
            ErrorCode::InvalidMarketConfig
        );
        Ok(())
    }

    pub fn eligible(&self) -> Result<bool> {
        self.validate()?;
        Ok(self.debt > 0
            && (self.debt >= self.collateral_value
                || u128::from(self.collateral_value - self.debt) * u128::from(BPS_DENOMINATOR)
                    <= u128::from(self.collateral_value) * u128::from(self.maintenance_bps)))
    }

    pub fn emergency_allowed(&self) -> Result<bool> {
        self.validate()?;
        Ok(self.debt > 0
            && (self.debt >= self.collateral_value
                || u128::from(self.collateral_value - self.debt) * u128::from(BPS_DENOMINATOR) * 100
                    <= u128::from(self.collateral_value) * u128::from(self.maintenance_bps) * 70))
    }

    /// Fractional progress from maintenance to nonpositive equity, scaled by
    /// 10,000. Saturation makes an insolvent position immediately reach the cap.
    pub fn distress_progress_bps(&self) -> Result<u16> {
        self.validate()?;
        if self.debt == 0 {
            return Ok(0);
        }
        if self.debt >= self.collateral_value {
            return Ok(BPS_DENOMINATOR);
        }
        let required = u128::from(self.collateral_value) * u128::from(self.maintenance_bps);
        let equity = u128::from(self.collateral_value - self.debt) * u128::from(BPS_DENOMINATOR);
        Ok((required.saturating_sub(equity) * u128::from(BPS_DENOMINATOR) / required) as u16)
    }

    pub fn rates(&self) -> Result<LiquidationRates> {
        require!(self.eligible()?, ErrorCode::PositionNotLiquidatable);
        let progress = u64::from(self.distress_progress_bps()?);
        Ok(LiquidationRates {
            buyer_discount_bps: LIQUIDATION_MINIMUM_DISCOUNT_BPS + (450 * progress / 10_000) as u16,
            emergency_reward_bps: (100 * progress / 10_000) as u16,
        })
    }

    /// Compare equity minus the surviving position's maintenance requirement.
    /// Euclidean quotient/remainder comparison preserves fractional improvement
    /// without a potentially overflowing signed 128-bit triple product.
    pub fn improves(&self, after: Self) -> Result<bool> {
        self.validate()?;
        after.validate()?;
        if after.debt == 0 {
            return Ok(true);
        }
        if self.collateral_value == 0 || after.collateral_value == 0 {
            return Ok(false);
        }
        if self.maintenance_bps == after.maintenance_bps {
            return Ok(u128::from(self.debt) * u128::from(after.collateral_value)
                > u128::from(after.debt) * u128::from(self.collateral_value));
        }
        let score = |health: Self| {
            let value = i128::from(health.collateral_value);
            let numerator = value * i128::from(BPS_DENOMINATOR - health.maintenance_bps)
                - i128::from(health.debt) * i128::from(BPS_DENOMINATOR);
            (numerator.div_euclid(value), numerator.rem_euclid(value) as u128)
        };
        let before_score = score(*self);
        let after_score = score(after);
        Ok(after_score.0 > before_score.0
            || (after_score.0 == before_score.0
                && after_score.1 * u128::from(self.collateral_value)
                    > before_score.1 * u128::from(after.collateral_value)))
    }

    pub fn recovered(&self) -> Result<bool> {
        self.validate()?;
        if self.debt == 0 {
            return Ok(true);
        }
        if self.debt >= self.collateral_value {
            return Ok(false);
        }
        let target = u32::from(self.maintenance_bps) + u32::from(LIQUIDATION_RECOVERY_BUFFER_BPS);
        Ok(u128::from(self.collateral_value - self.debt) * 10_000
            >= u128::from(self.collateral_value) * u128::from(target))
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct LiquidationRates {
    pub buyer_discount_bps: u16,
    pub emergency_reward_bps: u16,
}

/// Whole spendable atoms. Transfer-fee gross-ups are separate from the net fee
/// obligation. The collected fee is never counted again as debt repayment.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct LiquidationFeeAllocation {
    pub total: u64,
    pub protocol: u64,
    pub junior: u64,
}

impl LiquidationFeeAllocation {
    /// A fixed 3% charge, bounded by recovered surplus. The protocol share is
    /// allocated first; all underwriting revenue belongs to junior capital.
    pub fn quote(net_repayment: u64, surplus_cap: u64) -> Self {
        let total = (u128::from(net_repayment) * u128::from(LIQUIDATION_FEE_BPS) / u128::from(BPS_DENOMINATOR))
            .min(u128::from(surplus_cap)) as u64;
        let protocol =
            (u128::from(total) * u128::from(LIQUIDATION_PROTOCOL_SHARE_BPS) / u128::from(BPS_DENOMINATOR)) as u64;
        Self {
            total,
            protocol,
            junior: total - protocol,
        }
    }
}

/// A completed debt leg. `sale_recovery` is net of the caller reward and actual
/// transfer costs. Unpaid borrower/leverage interest is canceled.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct LiquidationLossAllocation {
    pub principal_repaid: u64,
    pub interest_paid: u64,
    pub principal_written_off: u64,
    pub interest_canceled: u64,
    pub surplus: u64,
}

impl LiquidationLossAllocation {
    /// Recovery pays principal first. The remaining funded credit loss is
    /// allocated to junior and then senior by the market tranche ledger.
    /// Unpaid borrower interest is canceled, never converted to principal loss.
    pub fn quote(principal: u64, debt: u64, sale_recovery: u64) -> Result<Self> {
        require_gte!(debt, principal, ErrorCode::BrokenInvariant);
        let principal_repaid = principal.min(sale_recovery);
        let interest = debt - principal;
        let interest_paid = interest.min(sale_recovery.saturating_sub(principal));
        Ok(Self {
            principal_repaid,
            interest_paid,
            principal_written_off: principal - principal_repaid,
            interest_canceled: interest - interest_paid,
            surplus: sale_recovery.saturating_sub(debt),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CollateralReceipt {
    pub collateral_credit: u64,
    pub collateral_debit: u64,
    pub base_collateral: u64,
    pub quote_collateral: u64,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub base_liquidation_cf_bps: u16,
    pub quote_liquidation_cf_bps: u16,
}

#[account]
#[derive(InitSpace, Default)]
pub struct BorrowPosition {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub position_id: Pubkey,
    pub base_collateral: u64,
    pub quote_collateral: u64,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub base_liquidation_cf_bps: u16,
    pub quote_liquidation_cf_bps: u16,
    pub base_referral_partner: Pubkey,
    pub quote_referral_partner: Pubkey,
    pub base_referral_interest_share_bps: u16,
    pub quote_referral_interest_share_bps: u16,
    pub fixed_base_shares: u128,
    pub fixed_quote_shares: u128,
    /// Original base borrowed by this position that has not been repaid or written off.
    pub fixed_base_principal: u64,
    /// Original quote borrowed by this position that has not been repaid or written off.
    pub fixed_quote_principal: u64,
    pub active_liquidation_session: Pubkey,

    pub auction_debt_asset: u8,
    pub auction_start_time: i64,
    pub auction_start_price_nad: u64,
    pub auction_floor_price_nad: u64,
    pub bump: u8,
}

impl BorrowPosition {
    pub fn initialize(&mut self, owner: Pubkey, market: Pubkey, position_id: Pubkey, bump: u8) {
        self.owner = owner;
        self.market = market;
        self.position_id = position_id;
        self.active_liquidation_session = Pubkey::default();

        self.clear_liquidation_auction();
        self.base_liquidation_cf_bps = 0;
        self.quote_liquidation_cf_bps = 0;
        self.base_referral_partner = Pubkey::default();
        self.quote_referral_partner = Pubkey::default();
        self.base_referral_interest_share_bps = 0;
        self.quote_referral_interest_share_bps = 0;
        self.bump = bump;
    }

    pub fn is_initialized(&self) -> bool {
        self.owner != Pubkey::default() && self.market != Pubkey::default()
    }

    /// Returns true only after every user balance, debt share, cached risk
    /// contribution, referral binding, and liquidation-auction field has been
    /// cleared. Terminal instructions may use this to refund the position's
    /// rent without turning account closure into a separate privileged path.
    pub fn is_empty(&self) -> bool {
        self.base_collateral == 0
            && self.quote_collateral == 0
            && self.global_health_base_contribution_for_quote_debt == 0
            && self.global_health_quote_contribution_for_base_debt == 0
            && self.base_liquidation_cf_bps == 0
            && self.quote_liquidation_cf_bps == 0
            && self.base_referral_partner == Pubkey::default()
            && self.quote_referral_partner == Pubkey::default()
            && self.base_referral_interest_share_bps == 0
            && self.quote_referral_interest_share_bps == 0
            && self.fixed_base_shares == 0
            && self.fixed_quote_shares == 0
            && self.fixed_base_principal == 0
            && self.fixed_quote_principal == 0
            && self.active_liquidation_session == Pubkey::default()
            && !self.has_active_liquidation_auction()
            && self.auction_start_time == 0
            && self.auction_start_price_nad == 0
            && self.auction_floor_price_nad == 0
    }

    pub fn assert_position(&self, owner: Pubkey, market: Pubkey) -> Result<()> {
        require_keys_eq!(self.owner, owner, ErrorCode::InvalidPositionMarket);
        require_keys_eq!(self.market, market, ErrorCode::InvalidPositionMarket);
        Ok(())
    }

    pub fn fixed_base_debt(&self, debt: &Debt) -> Result<u128> {
        Debt::shares_to_debt(self.fixed_base_shares, debt.base_borrow_index_nad)
    }

    pub fn require_idle(&self) -> Result<()> {
        super::liquidation_session::require_no_active_liquidation_session(self.active_liquidation_session)
    }

    pub fn debt_shares(&self, asset: MarketAsset) -> u128 {
        match asset {
            MarketAsset::Base => self.fixed_base_shares,
            MarketAsset::Quote => self.fixed_quote_shares,
        }
    }

    pub fn principal(&self, asset: MarketAsset) -> u64 {
        match asset {
            MarketAsset::Base => self.fixed_base_principal,
            MarketAsset::Quote => self.fixed_quote_principal,
        }
    }

    pub fn principal_mut(&mut self, asset: MarketAsset) -> &mut u64 {
        match asset {
            MarketAsset::Base => &mut self.fixed_base_principal,
            MarketAsset::Quote => &mut self.fixed_quote_principal,
        }
    }

    /// Allocate a share burn against this borrower's own original loan. The
    /// aggregate debt delta is the canonical cash charge; its floor phase can
    /// differ from the position's displayed debt by one atom. A full burn
    /// always clears this position's entire principal.
    pub fn principal_for_share_burn(
        &self,
        debt: &Debt,
        asset: MarketAsset,
        shares_to_burn: u128,
        aggregate_debt_reduction: u64,
    ) -> Result<u64> {
        let shares = self.debt_shares(asset);
        require!(
            shares_to_burn > 0 && shares_to_burn <= shares,
            ErrorCode::InsufficientDebt
        );
        let principal = self.principal(asset);
        if shares_to_burn == shares {
            require_gte!(aggregate_debt_reduction, principal, ErrorCode::DebtMathOverflow);
            return Ok(principal);
        }
        let position_debt = Debt::shares_to_debt(shares, debt.borrow_index(asset))?;
        require!(position_debt > 0, ErrorCode::DebtMathOverflow);
        require_gte!(position_debt, u128::from(principal), ErrorCode::DebtMathOverflow);
        let remaining_debt = Debt::shares_to_debt(shares - shares_to_burn, debt.borrow_index(asset))?;
        let amount = position_debt - remaining_debt;
        // Principal belongs to this position. The aggregate cash floor can
        // differ by one atom; using it here can leave principal above the
        // remaining position debt and prevent a later full repayment.
        let reduced = u128::from(principal)
            .checked_mul(amount)
            .and_then(|value| value.checked_div(position_debt))
            .ok_or(ErrorCode::DebtMathOverflow)?
            .min(u128::from(principal));
        u64::try_from(reduced).map_err(|_| ErrorCode::DebtMathOverflow.into())
    }

    pub fn fixed_quote_debt(&self, debt: &Debt) -> Result<u128> {
        Debt::shares_to_debt(self.fixed_quote_shares, debt.quote_borrow_index_nad)
    }

    pub fn liquidation_cf_bps(&self, debt_asset: MarketAsset) -> u16 {
        match debt_asset {
            MarketAsset::Base => self.base_liquidation_cf_bps,
            MarketAsset::Quote => self.quote_liquidation_cf_bps,
        }
    }

    pub fn set_liquidation_cf_bps(&mut self, debt_asset: MarketAsset, liquidation_cf_bps: u16) {
        match debt_asset {
            MarketAsset::Base => self.base_liquidation_cf_bps = liquidation_cf_bps,
            MarketAsset::Quote => self.quote_liquidation_cf_bps = liquidation_cf_bps,
        }
    }

    pub fn referral_partner(&self, debt_asset: MarketAsset) -> Pubkey {
        match debt_asset {
            MarketAsset::Base => self.base_referral_partner,
            MarketAsset::Quote => self.quote_referral_partner,
        }
    }

    pub fn referral_interest_share_bps(&self, debt_asset: MarketAsset) -> u16 {
        match debt_asset {
            MarketAsset::Base => self.base_referral_interest_share_bps,
            MarketAsset::Quote => self.quote_referral_interest_share_bps,
        }
    }

    pub fn set_referral_binding(&mut self, debt_asset: MarketAsset, referral_partner: Pubkey, interest_share_bps: u16) {
        match debt_asset {
            MarketAsset::Base => {
                self.base_referral_partner = referral_partner;
                self.base_referral_interest_share_bps = interest_share_bps;
            }
            MarketAsset::Quote => {
                self.quote_referral_partner = referral_partner;
                self.quote_referral_interest_share_bps = interest_share_bps;
            }
        }
    }

    pub fn clear_referral_binding(&mut self, debt_asset: MarketAsset) {
        self.set_referral_binding(debt_asset, Pubkey::default(), 0);
    }

    pub fn global_health_contribution(&self, debt_asset: MarketAsset) -> u64 {
        match debt_asset {
            MarketAsset::Base => self.global_health_quote_contribution_for_base_debt,
            MarketAsset::Quote => self.global_health_base_contribution_for_quote_debt,
        }
    }

    pub fn collateral(&self, asset: MarketAsset) -> u64 {
        match asset {
            MarketAsset::Base => self.base_collateral,
            MarketAsset::Quote => self.quote_collateral,
        }
    }

    pub fn has_active_liquidation_auction(&self) -> bool {
        self.auction_debt_asset != u8::MAX
    }

    pub fn active_liquidation_auction_asset(&self) -> Result<Option<MarketAsset>> {
        if !self.has_active_liquidation_auction() {
            require_eq!(self.auction_start_time, 0, ErrorCode::BrokenInvariant);
            return Ok(None);
        }
        require!(self.auction_start_time > 0, ErrorCode::BrokenInvariant);
        MarketAsset::try_from_code(self.auction_debt_asset).map(Some)
    }

    pub fn clear_liquidation_auction(&mut self) {
        self.auction_debt_asset = u8::MAX;
        self.auction_start_time = 0;
        self.auction_start_price_nad = 0;
        self.auction_floor_price_nad = 0;
    }
}

#[cfg(any(test, feature = "benchmark"))]
include!("../../benchmark/analysis/borrow_position_auction.rs");

#[cfg(test)]
mod tests {
    include!("../tests/state/borrow_position.rs");
}

#[cfg(test)]
mod liquidation_policy_tests {
    include!("../tests/transitions/liquidation_policy.rs");
}
