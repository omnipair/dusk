use crate::{
    constants::{BPS_DENOMINATOR, NAD},
    math::{ceil_div, mul_div_ceil_u128},
};
use anchor_lang::prelude::*;

use crate::{
    errors::ErrorCode,
    state::market::{Debt, MarketAsset},
};

#[account]
#[derive(InitSpace)]
pub struct LeveragePosition {
    pub owner: Pubkey,
    pub market: Pubkey,
    /// Funding signer that established this position. Ordinary opens use the
    /// owner; sponsored entry orders use their owner-created order PDA.
    pub namespace_authority: Pubkey,
    pub position_id: Pubkey,
    pub referral_partner: Pubkey,
    pub referral_interest_share_bps: u16,
    pub debt_asset: u8,
    pub collateral_amount: u64,
    pub margin_terms: LeverageMarginTerms,
    /// Entry equity valued in debt-token atoms.
    pub margin_amount: u64,
    pub open_notional: u64,
    pub debt_principal: u128,
    pub debt_shares: u128,
    pub multiplier_bps: u64,
    pub opened_at: i64,
    pub opened_slot: u64,
    /// Market curve revision committed by this opening. A recreated PDA gets
    /// a new revision even if it reopens in the same slot.
    pub open_curve_revision: u64,
    pub active_liquidation_session: Pubkey,
    pub distress: crate::state::LiquidationDistress,
    pub bump: u8,
}

impl LeveragePosition {
    pub fn initialize(
        &mut self,
        owner: Pubkey,
        market: Pubkey,
        namespace_authority: Pubkey,
        position_id: Pubkey,
        referral_partner: Pubkey,
        referral_interest_share_bps: u16,
        debt_asset: MarketAsset,
        collateral_amount: u64,
        margin_amount: u64,
        open_notional: u64,
        debt_principal: u64,
        debt_shares: u128,
        multiplier_bps: u64,
        opened_at: i64,
        opened_slot: u64,
        bump: u8,
    ) {
        self.owner = owner;
        self.market = market;
        self.namespace_authority = namespace_authority;
        self.position_id = position_id;
        self.referral_partner = referral_partner;
        self.referral_interest_share_bps = referral_interest_share_bps;
        self.debt_asset = debt_asset.code();
        self.collateral_amount = collateral_amount;
        self.margin_terms = LeverageMarginTerms::default();
        self.margin_amount = margin_amount;
        self.open_notional = open_notional;
        self.debt_principal = debt_principal as u128;
        self.debt_shares = debt_shares;
        self.multiplier_bps = multiplier_bps;
        self.opened_at = opened_at;
        self.opened_slot = opened_slot;
        self.active_liquidation_session = Pubkey::default();
        self.distress = crate::state::LiquidationDistress::default();
        self.bump = bump;
    }

    pub fn is_initialized(&self) -> bool {
        self.owner != Pubkey::default() && self.market != Pubkey::default()
    }

    pub fn assert_position(&self, owner: Pubkey, market: Pubkey, debt_asset: MarketAsset) -> Result<()> {
        require_keys_eq!(self.owner, owner, ErrorCode::InvalidLeveragePosition);
        require_keys_eq!(self.market, market, ErrorCode::InvalidLeveragePosition);
        require!(self.debt_asset()? == debt_asset, ErrorCode::InvalidLeveragePosition);
        Ok(())
    }

    pub fn debt_asset(&self) -> Result<MarketAsset> {
        MarketAsset::try_from_code(self.debt_asset)
    }

    pub fn collateral_asset(&self) -> Result<MarketAsset> {
        Ok(self.debt_asset()?.opposite())
    }

    pub fn debt_amount(&self, debt: &Debt) -> Result<u64> {
        let amount = Debt::shares_to_debt(self.debt_shares, debt.borrow_index(self.debt_asset()?))?;
        u64::try_from(amount).map_err(|_| ErrorCode::DebtMathOverflow.into())
    }

    pub fn require_open(&self) -> Result<()> {
        self.require_idle()?;
        require!(self.debt_shares > 0, ErrorCode::ZeroDebtAmount);
        require!(self.collateral_amount > 0, ErrorCode::InsufficientAmount);
        Ok(())
    }

    pub fn require_idle(&self) -> Result<()> {
        require_keys_eq!(
            self.active_liquidation_session,
            Pubkey::default(),
            ErrorCode::LiquidationSessionActive
        );
        Ok(())
    }

    pub fn credit_collateral(&mut self, amount: u64) -> Result<()> {
        self.require_idle()?;
        require!(amount > 0, ErrorCode::AmountZero);
        self.collateral_amount = self
            .collateral_amount
            .checked_add(amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }

    pub fn debit_collateral(&mut self, amount: u64) -> Result<()> {
        self.require_idle()?;
        require!(amount > 0, ErrorCode::AmountZero);
        self.collateral_amount = self
            .collateral_amount
            .checked_sub(amount)
            .ok_or(ErrorCode::InsufficientAmount)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    include!("../tests/state/leverage_position.rs");
}

/// Entry terms are snapshotted in collateral units. Price, LP activity and
/// another trader's position cannot passively rebase an existing position.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct LeverageMarginTerms {
    pub reference_collateral: u64,
    pub maintenance_boundaries_bps: [u16; 2],
    pub maintenance_rates_bps: [u16; 3],
    pub entry_buffer_bps: u16,
    /// Collateral atoms times NAD, not a fixed debt-token or dollar amount.
    /// Retained on equity extraction; released proportionally on size reduction.
    pub admission_equity_collateral_nad: u128,
}

impl LeverageMarginTerms {
    pub fn at_entry(reference_collateral: u64) -> Result<Self> {
        require_gt!(reference_collateral, 0, ErrorCode::InsufficientLiquidity);
        Ok(Self {
            reference_collateral,
            maintenance_boundaries_bps: [500, 1_500],
            maintenance_rates_bps: [700, 1_200, 1_700],
            entry_buffer_bps: 300,
            admission_equity_collateral_nad: 0,
        })
    }

    fn weighted_maintenance(&self, collateral: u64) -> Result<u128> {
        let [a, b] = self.maintenance_boundaries_bps;
        let [low, middle, high] = self.maintenance_rates_bps;
        require!(
            self.reference_collateral > 0
                && a > 0
                && b > a
                && low > 0
                && low <= middle
                && middle <= high
                && u32::from(high) + u32::from(self.entry_buffer_bps) < u32::from(BPS_DENOMINATOR),
            ErrorCode::InvalidMarketConfig
        );
        // Fractional band boundaries stay exact even for low-decimal tokens.
        let amount = u128::from(collateral) * u128::from(BPS_DENOMINATOR);
        let first = u128::from(self.reference_collateral) * u128::from(a);
        let second = u128::from(self.reference_collateral) * u128::from(b);
        Ok(amount.min(first) * u128::from(low)
            + amount.saturating_sub(first).min(second - first) * u128::from(middle)
            + amount.saturating_sub(second) * u128::from(high))
    }

    pub fn maintenance_bps(&self, collateral: u64) -> Result<u16> {
        require_gt!(collateral, 0, ErrorCode::AmountZero);
        let rate = ceil_div(
            self.weighted_maintenance(collateral)?,
            u128::from(collateral) * u128::from(BPS_DENOMINATOR),
        )
        .ok_or(ErrorCode::MarketMathOverflow)?;
        u16::try_from(rate).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub fn initial_bps(&self, collateral: u64) -> Result<u16> {
        let own = self
            .maintenance_bps(collateral)?
            .checked_add(self.entry_buffer_bps)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let retained = mul_div_ceil_u128(
            self.admission_equity_collateral_nad,
            u128::from(BPS_DENOMINATOR),
            u128::from(collateral) * u128::from(NAD),
        )?;
        require!(
            retained < u128::from(BPS_DENOMINATOR),
            ErrorCode::LeverageInitialMarginTooLow
        );
        Ok(own.max(u16::try_from(retained).map_err(|_| ErrorCode::MarketMathOverflow)?))
    }

    pub(crate) fn own_initial_equity_nad(&self, collateral: u64) -> Result<u128> {
        let bps = u128::from(BPS_DENOMINATOR);
        let weighted = self
            .weighted_maintenance(collateral)?
            .checked_add(u128::from(collateral) * bps * u128::from(self.entry_buffer_bps))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        mul_div_ceil_u128(weighted, u128::from(NAD), bps * bps)
    }
}

/// Cumulative equity in collateral atoms times NAD. The existing gentle
/// crowding curve supplies a rising tail beyond the progressive size bands.
/// Do not cap the potential: a marginal charge above all collateral must reject
/// new leverage, rather than become cheaper by crossing a saturated boundary.
pub(crate) fn leverage_equity_potential(exposure: u64, reference: u64) -> Result<u128> {
    let terms = LeverageMarginTerms::at_entry(reference)?;
    let own = terms.own_initial_equity_nad(exposure)?;
    let exposure_ratio_nad = mul_div_ceil_u128(u128::from(exposure), u128::from(NAD), u128::from(reference))?;
    let excess = exposure_ratio_nad.saturating_sub(u128::from(NAD) / 5);
    let rate = u128::from(NAD) / 10 + ceil_div(excess, 10).ok_or(ErrorCode::MarketMathOverflow)?;
    let crowding = u128::from(exposure)
        .checked_mul(rate)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(own.max(crowding))
}

#[cfg(test)]
mod margin_tests {
    include!("../tests/state/leverage_margin.rs");
}
