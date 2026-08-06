use anchor_lang::prelude::*;

use crate::constants::*;
use crate::errors::ErrorCode;
use crate::math::{
    adapt_rate_at_target_nad, denormalize_from_nad_floor, instantaneous_rate_apr_nad, normalize_to_nad,
    utilization_bps, utilization_error_nad,
};
use crate::shared::math::{ceil_div, SqrtU128};
use crate::state::{
    borrow_position::{BorrowPosition, CollateralReceipt},
    futarchy_authority::{FutarchyAuthority, ProtocolAuctionSplit},
    MarketParameterUpdate,
};

use super::{
    health::max_cf_bps_from_liquidation_cf, AmmCurveParameters, AmmState, Debt, DebtRepaymentQuote, FeesReceipt,
    HlpVault, MarketAsset, MarketConfig, MarketHealth, MarketSide, Risk, SwapFeeBreakdown, MAX_DAILY_BORROW_BPS,
};

#[cfg(test)]
use super::Reserves;

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DebtReceipt {
    pub debt_delta: i64,
    pub cash_repaid: u64,
    pub interest_paid: u64,
    pub fixed_base_debt: u128,
    pub fixed_quote_debt: u128,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub base_liquidation_cf_bps: u16,
    pub quote_liquidation_cf_bps: u16,
    pub base_debt_health_bps: u64,
    pub quote_debt_health_bps: u64,
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
    pub reserve_in_live_reserve: u64,
    pub reserve_out_live_reserve: u64,
    pub fees: FeesReceipt,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct Insurance {
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub base_available: u64,
    pub quote_available: u64,
}

impl DebtReceipt {
    fn from_market(
        market: &Market,
        borrow_position: &BorrowPosition,
        debt_delta: i64,
        cash_repaid: u64,
        interest_paid: u64,
        health: &MarketHealth,
    ) -> Result<Self> {
        Ok(Self {
            debt_delta,
            cash_repaid,
            interest_paid,
            fixed_base_debt: market.debt.fixed_base_debt()?,
            fixed_quote_debt: market.debt.fixed_quote_debt()?,
            global_health_base_contribution_for_quote_debt: borrow_position
                .global_health_base_contribution_for_quote_debt,
            global_health_quote_contribution_for_base_debt: borrow_position
                .global_health_quote_contribution_for_base_debt,
            base_liquidation_cf_bps: borrow_position.base_liquidation_cf_bps,
            quote_liquidation_cf_bps: borrow_position.quote_liquidation_cf_bps,
            base_debt_health_bps: health.base_debt_health_bps,
            quote_debt_health_bps: health.quote_debt_health_bps,
        })
    }
}

#[account]
#[derive(InitSpace, Default)]
pub struct Market {
    pub version: u8,
    pub ylp_mint: Pubkey,
    pub base_side: MarketSide,
    pub quote_side: MarketSide,
    pub config: MarketConfig,
    pub amm: AmmState,
    pub debt: Debt,
    pub base_hlp_vault: HlpVault,
    pub quote_hlp_vault: HlpVault,
    pub risk: Risk,
    pub insurance: Insurance,
    pub params_hash: [u8; 32],
    /// External yLP burned into active governance support. This is added back
    /// when computing direct-yLP eligibility; internal reserve-share supply is
    /// intentionally unchanged by governance locking.
    pub governance_locked_ylp: u64,
    /// Independent monotone revisions for fee, concentration, IRM, EMA, and
    /// daily-borrow-limit parameter families, in that order.
    pub parameter_revisions: [u64; 5],
    /// Latest trader-visible marginal price committed by a curve mutation.
    pub last_marginal_observation_nad: u64,
    /// Monotone revision for executable-curve mutations.
    pub curve_revision: u64,
    /// Curve revision represented by the materialized lending-risk snapshot.
    pub risk_revision: u64,
    pub last_update_slot: u64,
    pub reduce_only: bool,
    pub bump: u8,
}

impl Market {
    pub(crate) fn validate_mint_domain(
        base_asset_mint: Pubkey,
        quote_asset_mint: Pubkey,
        ylp_mint: Pubkey,
        base_hlp_mint: Pubkey,
        quote_hlp_mint: Pubkey,
    ) -> Result<()> {
        require_keys_neq!(base_asset_mint, quote_asset_mint, ErrorCode::InvalidMint);
        require_keys_neq!(ylp_mint, base_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(ylp_mint, quote_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(base_hlp_mint, base_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(base_hlp_mint, quote_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(base_hlp_mint, ylp_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(quote_hlp_mint, base_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(quote_hlp_mint, quote_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(quote_hlp_mint, ylp_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(quote_hlp_mint, base_hlp_mint, ErrorCode::InvalidLpMintKey);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        &mut self,
        ylp_mint: Pubkey,
        base_side: MarketSide,
        quote_side: MarketSide,
        config: MarketConfig,
        base_hlp_ylp_vault: Pubkey,
        quote_hlp_ylp_vault: Pubkey,
        base_insurance_vault: Pubkey,
        quote_insurance_vault: Pubkey,
        params_hash: [u8; 32],
        current_slot: u64,
        bump: u8,
    ) -> Result<()> {
        config.validate()?;
        Self::validate_mint_domain(
            base_side.asset_mint,
            quote_side.asset_mint,
            ylp_mint,
            base_side.hlp_mint,
            quote_side.hlp_mint,
        )?;
        self.version = MARKET_LAYOUT_VERSION;
        self.ylp_mint = ylp_mint;
        self.base_side = base_side;
        self.quote_side = quote_side;
        self.config = config;
        // Reserves are empty at market creation. The first balanced-liquidity
        // checkpoint initializes center, Q/share, and the applied parameters.
        self.amm = AmmState::default();
        self.debt = Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            base_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            quote_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            base_last_accrual_slot: current_slot,
            quote_last_accrual_slot: current_slot,
            ..Debt::default()
        };
        self.base_hlp_vault = {
            let mut vault = HlpVault::default();
            vault.initialize(base_hlp_ylp_vault);
            vault
        };
        self.quote_hlp_vault = {
            let mut vault = HlpVault::default();
            vault.initialize(quote_hlp_ylp_vault);
            vault
        };
        self.risk = Risk {
            last_snapshot_slot: current_slot,
            ..Risk::default()
        };
        self.insurance = Insurance {
            base_vault: base_insurance_vault,
            quote_vault: quote_insurance_vault,
            ..Insurance::default()
        };
        self.params_hash = params_hash;
        self.governance_locked_ylp = 0;
        self.parameter_revisions = [0; 5];
        self.last_marginal_observation_nad = 0;
        self.curve_revision = 0;
        self.risk_revision = 0;
        self.last_update_slot = current_slot;
        self.reduce_only = false;
        self.bump = bump;
        Ok(())
    }

    pub fn assert_live_with_futarchy(&self, futarchy_authority: &FutarchyAuthority) -> Result<()> {
        self.assert_live_with_futarchy_at(futarchy_authority, Clock::get()?.unix_timestamp)
    }

    pub(crate) fn assert_live_with_futarchy_at(
        &self,
        futarchy_authority: &FutarchyAuthority,
        unix_timestamp: i64,
    ) -> Result<()> {
        self.assert_started_at(unix_timestamp)?;
        require!(
            !futarchy_authority.is_reduce_only(self.reduce_only),
            ErrorCode::ReduceOnlyMode
        );
        Ok(())
    }

    pub fn assert_current_version(&self) -> Result<()> {
        require_eq!(self.version, MARKET_LAYOUT_VERSION, ErrorCode::InvalidVersion);
        Ok(())
    }

    pub fn assert_started(&self) -> Result<()> {
        self.assert_started_at(Clock::get()?.unix_timestamp)
    }

    pub(crate) fn assert_started_at(&self, unix_timestamp: i64) -> Result<()> {
        self.assert_current_version()?;
        require!(unix_timestamp >= self.config.start_time, ErrorCode::MarketNotStarted);
        Ok(())
    }

    /// Accrue borrow interest up to the current slot. Should be called before any
    /// debt-dependent computation in an instruction (borrow/repay, hedge,
    /// liquidation, yield claims, swaps, and liquidity changes).
    pub fn accrue_interest(&mut self) -> Result<()> {
        let current_slot = Clock::get()?.slot;
        self.accrue_interest_to_slot(current_slot)
    }

    pub fn update(&mut self) -> Result<()> {
        self.assert_current_version()?;
        let current_slot = Clock::get()?.slot;
        self.accrue_interest_to_slot(current_slot)?;
        if self.base_side.reserves.live_reserve > 0 && self.quote_side.reserves.live_reserve > 0 {
            // hLP exposure is checkpointed from actual state. New hLP entry
            // remains gated while a due concentrated controller target would
            // otherwise price the mint against a stale NAV basis.
            self.advance_amm_clock(current_slot)?;
            self.checkpoint_hlp_vaults()?;
            self.refresh_risk()?;
        }
        Ok(())
    }

    /// Advances debt, controller clocks, and hLP accounting for leverage
    /// margin changes without eagerly rebuilding risk that the transition will
    /// immediately invalidate. The transition records its final exact risk
    /// observation after the reserve/debt mutation.
    pub(crate) fn prepare_leverage_margin_operation(&mut self, current_slot: u64) -> Result<()> {
        self.assert_current_version()?;
        self.accrue_interest_to_slot(current_slot)?;
        if self.base_side.reserves.live_reserve > 0 && self.quote_side.reserves.live_reserve > 0 {
            self.advance_amm_clock(current_slot)?;
            self.checkpoint_hlp_vaults()?;
        }
        Ok(())
    }

    pub(crate) fn accrue_interest_to_slot(&mut self, current_slot: u64) -> Result<()> {
        accrue_side(self, MarketAsset::Base, current_slot)?;
        accrue_side(self, MarketAsset::Quote, current_slot)?;
        Ok(())
    }

    /// Validate one typed governance action without permitting unrelated
    /// configuration fields to move with it.
    pub fn validate_parameter_update(&self, update: &MarketParameterUpdate) -> Result<()> {
        match update {
            MarketParameterUpdate::Fee(profile) => {
                profile.validate()?;
                require!(
                    self.config.fee_profile() != *profile,
                    ErrorCode::ParameterUpdateNotMeaningful
                );
            }
            MarketParameterUpdate::Concentration {
                peak_depth_nad,
                fade_scale_nad,
                ramp_duration_slots,
            } => {
                let target = AmmCurveParameters {
                    peak_depth_nad: *peak_depth_nad,
                    fade_scale_nad: *fade_scale_nad,
                };
                target.validate_endpoint()?;
                require!(
                    (super::MIN_AMM_RAMP_DURATION_SLOTS..=super::MAX_AMM_RAMP_DURATION_SLOTS)
                        .contains(ramp_duration_slots),
                    ErrorCode::InvalidParameterUpdate
                );
                require!(
                    self.config.amm.curve_parameters() != target,
                    ErrorCode::ParameterUpdateNotMeaningful
                );
            }
            MarketParameterUpdate::Irm(irm) => {
                irm.validate()?;
                require!(self.config.irm != *irm, ErrorCode::ParameterUpdateNotMeaningful);
            }
            MarketParameterUpdate::EmaHalfLives {
                price_ms,
                directional_price_ms,
                q_ms,
                center_price_ms,
            } => {
                require!(
                    (MIN_HALF_LIFE_MS..=MAX_HALF_LIFE_MS).contains(price_ms)
                        && (MIN_HALF_LIFE_MS..=MAX_HALF_LIFE_MS).contains(directional_price_ms)
                        && (MIN_HALF_LIFE_MS..=MAX_HALF_LIFE_MS).contains(q_ms)
                        && (MIN_HALF_LIFE_MS..=MAX_HALF_LIFE_MS).contains(center_price_ms),
                    ErrorCode::InvalidHalfLife
                );
                require!(
                    self.config.ema_half_life_ms != *price_ms
                        || self.config.directional_ema_half_life_ms != *directional_price_ms
                        || self.config.q_ema_half_life_ms != *q_ms
                        || self.config.amm.center_ema_half_life_ms != *center_price_ms,
                    ErrorCode::ParameterUpdateNotMeaningful
                );
            }
            MarketParameterUpdate::DailyBorrowLimit { max_daily_borrow_bps } => {
                require!(
                    *max_daily_borrow_bps <= MAX_DAILY_BORROW_BPS,
                    ErrorCode::InvalidParameterUpdate
                );
                require!(
                    self.config.max_daily_borrow_bps != *max_daily_borrow_bps,
                    ErrorCode::ParameterUpdateNotMeaningful
                );
            }
        }
        Ok(())
    }

    /// Checkpoint all elapsed state under the old parameters, apply exactly
    /// one typed family, enforce the point-in-time utilization guard, and then
    /// advance only that family's revision.
    pub fn execute_parameter_update(&mut self, update: &MarketParameterUpdate, current_slot: u64) -> Result<()> {
        self.assert_current_version()?;
        self.validate_parameter_update(update)?;

        let previous_config = self.config;
        let previous_base_side = self.base_side;
        let previous_quote_side = self.quote_side;
        let previous_amm = self.amm;
        let previous_debt = self.debt;
        let previous_risk = self.risk;
        let previous_revisions = self.parameter_revisions;
        let previous_last_marginal_observation_nad = self.last_marginal_observation_nad;
        let previous_curve_revision = self.curve_revision;
        let previous_risk_revision = self.risk_revision;
        let previous_last_update_slot = self.last_update_slot;

        let apply_result = (|| {
            // No elapsed interest, EMA decay, or risk integration may be
            // retroactively evaluated under the newly selected parameters.
            self.accrue_interest_to_slot(current_slot)?;
            if self.amm.initialized {
                self.amm
                    .observe_clock_from_validated_config(&previous_config.amm, current_slot)?;
            }
            self.refresh_risk_at_slot(current_slot)?;
            self.assert_parameter_execution_utilization()?;

            let family_index = update.family().code() as usize;
            match update {
                MarketParameterUpdate::Fee(profile) => {
                    self.config.apply_fee_profile(*profile)?;
                    if self.amm.initialized {
                        self.amm.invalidate_deferred_controller_target();
                    }
                }
                MarketParameterUpdate::Concentration {
                    peak_depth_nad,
                    fade_scale_nad,
                    ramp_duration_slots,
                } => {
                    let applied = self.amm.effective_curve_parameters(&previous_config.amm, current_slot);
                    let mut next = self.config;
                    next.amm.peak_depth_nad = *peak_depth_nad;
                    next.amm.fade_scale_nad = *fade_scale_nad;
                    next.amm.ramp_duration_slots = *ramp_duration_slots;
                    next.validate()?;
                    self.config = next;
                    if self.amm.initialized {
                        self.amm.start_applied_ramp(applied, &self.config.amm, current_slot)?;
                    }
                }
                MarketParameterUpdate::Irm(irm) => {
                    let mut next = self.config;
                    next.irm = *irm;
                    next.validate()?;
                    self.config = next;
                }
                MarketParameterUpdate::EmaHalfLives {
                    price_ms,
                    directional_price_ms,
                    q_ms,
                    center_price_ms,
                } => {
                    let mut next = self.config;
                    next.ema_half_life_ms = *price_ms;
                    next.directional_ema_half_life_ms = *directional_price_ms;
                    next.q_ema_half_life_ms = *q_ms;
                    next.amm.center_ema_half_life_ms = *center_price_ms;
                    next.validate()?;
                    self.config = next;
                }
                MarketParameterUpdate::DailyBorrowLimit { max_daily_borrow_bps } => {
                    // Close elapsed refill under the old governed rate. The
                    // newly selected rate applies only from this slot onward.
                    let old_limit_bps = self.config.max_daily_borrow_bps;
                    let base_limit = self.daily_limit_for_side(MarketAsset::Base, old_limit_bps)?;
                    let quote_limit = self.daily_limit_for_side(MarketAsset::Quote, old_limit_bps)?;
                    self.base_side.daily_limits.decay_to_slot(base_limit, current_slot)?;
                    self.quote_side.daily_limits.decay_to_slot(quote_limit, current_slot)?;
                    let mut next = self.config;
                    next.max_daily_borrow_bps = *max_daily_borrow_bps;
                    next.validate()?;
                    self.config = next;
                }
            }

            self.finalize_amm_transition(current_slot)?;
            self.refresh_risk_at_slot(current_slot)?;
            self.assert_market_health()?;
            self.parameter_revisions[family_index] = self.parameter_revisions[family_index]
                .checked_add(1)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            Ok(())
        })();

        if apply_result.is_err() {
            self.config = previous_config;
            self.base_side = previous_base_side;
            self.quote_side = previous_quote_side;
            self.amm = previous_amm;
            self.debt = previous_debt;
            self.risk = previous_risk;
            self.parameter_revisions = previous_revisions;
            self.last_marginal_observation_nad = previous_last_marginal_observation_nad;
            self.curve_revision = previous_curve_revision;
            self.risk_revision = previous_risk_revision;
            self.last_update_slot = previous_last_update_slot;
        }
        apply_result
    }

    pub fn side(&self, market_asset: MarketAsset) -> &MarketSide {
        match market_asset {
            MarketAsset::Base => &self.base_side,
            MarketAsset::Quote => &self.quote_side,
        }
    }

    pub fn side_mut(&mut self, market_asset: MarketAsset) -> &mut MarketSide {
        match market_asset {
            MarketAsset::Base => &mut self.base_side,
            MarketAsset::Quote => &mut self.quote_side,
        }
    }

    pub fn asset_for_mint(&self, mint: Pubkey) -> Result<MarketAsset> {
        if mint == self.base_side.asset_mint {
            return Ok(MarketAsset::Base);
        }
        if mint == self.quote_side.asset_mint {
            return Ok(MarketAsset::Quote);
        }
        err!(ErrorCode::InvalidMint)
    }

    pub fn asset_for_hlp_mint(&self, mint: Pubkey) -> Result<MarketAsset> {
        if mint == self.base_side.hlp_mint {
            return Ok(MarketAsset::Base);
        }
        if mint == self.quote_side.hlp_mint {
            return Ok(MarketAsset::Quote);
        }
        err!(ErrorCode::InvalidLpMintKey)
    }

    pub fn swap_sides(&self, asset_in: MarketAsset) -> (&MarketSide, &MarketSide) {
        match asset_in {
            MarketAsset::Base => (&self.base_side, &self.quote_side),
            MarketAsset::Quote => (&self.quote_side, &self.base_side),
        }
    }

    pub fn swap_sides_mut(&mut self, asset_in: MarketAsset) -> (&mut MarketSide, &mut MarketSide) {
        match asset_in {
            MarketAsset::Base => (&mut self.base_side, &mut self.quote_side),
            MarketAsset::Quote => (&mut self.quote_side, &mut self.base_side),
        }
    }

    pub fn deposit_collateral(
        &mut self,
        borrow_position: &mut BorrowPosition,
        market_asset: MarketAsset,
        collateral_credit: u64,
    ) -> Result<CollateralReceipt> {
        require!(collateral_credit > 0, ErrorCode::AmountZero);
        let projected_collateral = borrow_position
            .collateral(market_asset)
            .checked_add(collateral_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let debt_asset = market_asset.opposite();
        let projected_debt = match debt_asset {
            MarketAsset::Base => borrow_position.fixed_base_debt(&self.debt)?,
            MarketAsset::Quote => borrow_position.fixed_quote_debt(&self.debt)?,
        };
        let target_contribution =
            self.debt_capped_global_health_contribution(debt_asset, projected_debt, projected_collateral, &self.risk)?;

        match market_asset {
            MarketAsset::Base => borrow_position.base_collateral = projected_collateral,
            MarketAsset::Quote => borrow_position.quote_collateral = projected_collateral,
        }
        self.reconcile_global_health_contribution(borrow_position, debt_asset, target_contribution)?;
        self.reconcile_liquidation_auction(borrow_position)?;

        Ok(CollateralReceipt {
            collateral_credit,
            collateral_debit: 0,
            base_collateral: borrow_position.base_collateral,
            quote_collateral: borrow_position.quote_collateral,
            global_health_base_contribution_for_quote_debt: borrow_position
                .global_health_base_contribution_for_quote_debt,
            global_health_quote_contribution_for_base_debt: borrow_position
                .global_health_quote_contribution_for_base_debt,
            base_liquidation_cf_bps: borrow_position.base_liquidation_cf_bps,
            quote_liquidation_cf_bps: borrow_position.quote_liquidation_cf_bps,
        })
    }

    pub fn withdraw_collateral(
        &mut self,
        borrow_position: &mut BorrowPosition,
        market_asset: MarketAsset,
        collateral_debit: u64,
        min_liquidation_cf_bps: u16,
    ) -> Result<CollateralReceipt> {
        require!(collateral_debit > 0, ErrorCode::AmountZero);
        let projected_collateral = borrow_position
            .collateral(market_asset)
            .checked_sub(collateral_debit)
            .ok_or(ErrorCode::InsufficientBalance)?;
        let debt_asset = market_asset.opposite();
        let position_debt = match debt_asset {
            MarketAsset::Base => borrow_position.fixed_base_debt(&self.debt)?,
            MarketAsset::Quote => borrow_position.fixed_quote_debt(&self.debt)?,
        };
        let target_contribution =
            self.debt_capped_global_health_contribution(debt_asset, position_debt, projected_collateral, &self.risk)?;

        if position_debt > 0 {
            let total_debt_nad = self.total_fixed_debt_nad(debt_asset)?;
            let external_debt_nad = self.external_fixed_debt_nad(borrow_position, debt_asset)?;
            let projected_aggregate =
                self.projected_aggregate_global_health_contribution(borrow_position, debt_asset, target_contribution)?;
            let terms = self.dynamic_borrow_terms(
                debt_asset,
                projected_collateral,
                external_debt_nad,
                total_debt_nad,
                projected_aggregate,
                &self.risk,
            )?;
            // A third party cannot lower this position's already-issued terms.
            // The owner may withdraw whenever the post-withdraw position remains
            // inside its stored 5% buffered liquidation CF.
            let liquidation_cf_bps = borrow_position
                .liquidation_cf_bps(debt_asset)
                .max(terms.liquidation_cf_bps);
            let collateral_value_nad = self.collateral_value_nad(market_asset, projected_collateral, &self.risk)?;
            let max_debt_nad = collateral_value_nad
                .checked_mul(max_cf_bps_from_liquidation_cf(liquidation_cf_bps) as u128)
                .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let max_debt = denormalize_from_nad_floor(max_debt_nad, self.side(market_asset.opposite()).asset_decimals)?;
            require_gte!(max_debt as u128, position_debt, ErrorCode::InsufficientMarketHealth);
            require_gte!(liquidation_cf_bps, min_liquidation_cf_bps, ErrorCode::SlippageExceeded);
            borrow_position.set_liquidation_cf_bps(debt_asset, liquidation_cf_bps);
        } else {
            borrow_position.set_liquidation_cf_bps(debt_asset, 0);
        }

        match market_asset {
            MarketAsset::Base => borrow_position.base_collateral = projected_collateral,
            MarketAsset::Quote => borrow_position.quote_collateral = projected_collateral,
        }
        self.reconcile_global_health_contribution(borrow_position, debt_asset, target_contribution)?;

        Ok(CollateralReceipt {
            collateral_credit: 0,
            collateral_debit,
            base_collateral: borrow_position.base_collateral,
            quote_collateral: borrow_position.quote_collateral,
            global_health_base_contribution_for_quote_debt: borrow_position
                .global_health_base_contribution_for_quote_debt,
            global_health_quote_contribution_for_base_debt: borrow_position
                .global_health_quote_contribution_for_base_debt,
            base_liquidation_cf_bps: borrow_position.base_liquidation_cf_bps,
            quote_liquidation_cf_bps: borrow_position.quote_liquidation_cf_bps,
        })
    }

    pub fn borrow(
        &mut self,
        borrow_position: &mut BorrowPosition,
        borrow_asset: MarketAsset,
        borrow_amount: u64,
        min_liquidation_cf_bps: u16,
        current_slot: u64,
    ) -> Result<DebtReceipt> {
        require!(borrow_amount > 0, ErrorCode::AmountZero);
        let debt_delta = i64::try_from(borrow_amount).map_err(|_| ErrorCode::Overflow)?;
        if self.risk.q_ema_nad == 0 {
            self.refresh_risk_at_slot(current_slot)?;
        }
        let risk = self.risk;
        let current_health = self.market_health_from_risk(&risk)?;
        self.assert_market_health_snapshot(&current_health)?;
        // The V1 curve prices debt already issued to other positions. Counting
        // this position's own debt here would make repeated draws worse than
        // opening equivalent split positions.
        let external_debt_nad = self.external_fixed_debt_nad(borrow_position, borrow_asset)?;
        let debt_shares = match borrow_asset {
            MarketAsset::Base => Debt::debt_to_shares(borrow_amount, self.debt.base_borrow_index_nad)?,
            MarketAsset::Quote => Debt::debt_to_shares(borrow_amount, self.debt.quote_borrow_index_nad)?,
        };
        let aggregate_debt_increase = self.debt.fixed_debt_increase_for_shares(borrow_asset, debt_shares)?;
        let (projected_position_debt, projected_total_debt) = match borrow_asset {
            MarketAsset::Base => (
                Debt::shares_to_debt(
                    borrow_position
                        .fixed_base_shares
                        .checked_add(debt_shares)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    self.debt.base_borrow_index_nad,
                )?,
                Debt::shares_to_debt(
                    self.debt
                        .fixed_base_shares
                        .checked_add(debt_shares)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    self.debt.base_borrow_index_nad,
                )?,
            ),
            MarketAsset::Quote => (
                Debt::shares_to_debt(
                    borrow_position
                        .fixed_quote_shares
                        .checked_add(debt_shares)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    self.debt.quote_borrow_index_nad,
                )?,
                Debt::shares_to_debt(
                    self.debt
                        .fixed_quote_shares
                        .checked_add(debt_shares)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    self.debt.quote_borrow_index_nad,
                )?,
            ),
        };
        let collateral_asset = borrow_asset.opposite();
        let collateral_amount = borrow_position.collateral(collateral_asset);
        let target_contribution = self.debt_capped_global_health_contribution(
            borrow_asset,
            projected_position_debt,
            collateral_amount,
            &risk,
        )?;
        let projected_aggregate =
            self.projected_aggregate_global_health_contribution(borrow_position, borrow_asset, target_contribution)?;
        let projected_total_debt_nad = normalize_to_nad(projected_total_debt, self.side(borrow_asset).asset_decimals)?;
        let terms = self.dynamic_borrow_terms(
            borrow_asset,
            collateral_amount,
            external_debt_nad,
            projected_total_debt_nad,
            projected_aggregate,
            &risk,
        )?;
        require_gte!(
            terms.max_debt as u128,
            projected_position_debt,
            ErrorCode::InsufficientMarketHealth
        );
        require_gte!(
            terms.liquidation_cf_bps,
            min_liquidation_cf_bps,
            ErrorCode::SlippageExceeded
        );
        require_gte!(
            terms.projected_market_health_bps,
            self.config.borrow_market_health_floor_bps as u64,
            ErrorCode::InsufficientMarketHealth
        );
        require_gte!(
            self.side(borrow_asset).reserves.cash_reserve,
            borrow_amount,
            ErrorCode::InsufficientBorrowHeadroom
        );
        self.record_new_borrow(borrow_asset, borrow_amount, current_slot)?;
        let debt_side = self.side_mut(borrow_asset);
        debt_side.reserves.cash_reserve = debt_side
            .reserves
            .cash_reserve
            .checked_sub(borrow_amount)
            .ok_or(ErrorCode::CashReserveUnderflow)?;
        if aggregate_debt_increase > borrow_amount {
            debt_side.reserves.live_reserve = debt_side
                .reserves
                .live_reserve
                .checked_add(aggregate_debt_increase - borrow_amount)
                .ok_or(ErrorCode::ReserveOverflow)?;
        } else if aggregate_debt_increase < borrow_amount {
            debt_side.reserves.live_reserve = debt_side
                .reserves
                .live_reserve
                .checked_sub(borrow_amount - aggregate_debt_increase)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }

        match borrow_asset {
            MarketAsset::Base => {
                borrow_position.fixed_base_shares = borrow_position
                    .fixed_base_shares
                    .checked_add(debt_shares)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.debt.fixed_base_shares = self
                    .debt
                    .fixed_base_shares
                    .checked_add(debt_shares)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
            MarketAsset::Quote => {
                borrow_position.fixed_quote_shares = borrow_position
                    .fixed_quote_shares
                    .checked_add(debt_shares)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.debt.fixed_quote_shares = self
                    .debt
                    .fixed_quote_shares
                    .checked_add(debt_shares)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
        }
        self.debt.add_margin_principal(borrow_asset, borrow_amount)?;
        self.reconcile_global_health_contribution(borrow_position, borrow_asset, target_contribution)?;
        borrow_position.set_liquidation_cf_bps(borrow_asset, terms.liquidation_cf_bps);
        let market_health = self.market_health()?;
        DebtReceipt::from_market(self, borrow_position, debt_delta, 0, 0, &market_health)
    }

    pub(crate) fn projected_aggregate_global_health_contribution(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        target_contribution: u64,
    ) -> Result<u64> {
        let (position_contribution, aggregate_contribution) = match debt_asset {
            MarketAsset::Base => (
                borrow_position.global_health_quote_contribution_for_base_debt,
                self.debt.global_health_quote_contribution_for_base_debt,
            ),
            MarketAsset::Quote => (
                borrow_position.global_health_base_contribution_for_quote_debt,
                self.debt.global_health_base_contribution_for_quote_debt,
            ),
        };
        aggregate_contribution
            .checked_sub(position_contribution)
            .and_then(|value| value.checked_add(target_contribution))
            .ok_or(ErrorCode::MarketMathOverflow.into())
    }

    pub(crate) fn reconcile_global_health_contribution(
        &mut self,
        borrow_position: &mut BorrowPosition,
        debt_asset: MarketAsset,
        target_contribution: u64,
    ) -> Result<()> {
        match debt_asset {
            MarketAsset::Base => reconcile_global_health_contribution(
                &mut borrow_position.global_health_quote_contribution_for_base_debt,
                &mut self.debt.global_health_quote_contribution_for_base_debt,
                target_contribution,
            ),
            MarketAsset::Quote => reconcile_global_health_contribution(
                &mut borrow_position.global_health_base_contribution_for_quote_debt,
                &mut self.debt.global_health_base_contribution_for_quote_debt,
                target_contribution,
            ),
        }
    }

    pub fn repay(
        &mut self,
        borrow_position: &mut BorrowPosition,
        repay_asset: MarketAsset,
        repay_credit: u64,
    ) -> Result<DebtReceipt> {
        let repayment = self.fixed_repayment_for_max(borrow_position, repay_asset, repay_credit)?;
        // Instruction handlers preview this amount before moving tokens. Keep
        // the state boundary exact so no transferred atom can become an
        // unaccounted donation if state changed unexpectedly.
        require_eq!(repayment.cash_repaid, repay_credit, ErrorCode::BrokenInvariant);
        let (interest_paid, debt_reduction) = match repay_asset {
            MarketAsset::Base => {
                let shares_to_burn = repayment.shares_to_burn;
                let debt_reduction = repayment.position_debt_reduced;
                let aggregate_debt_reduction =
                    self.debt.fixed_debt_reduction_for_shares(repay_asset, shares_to_burn)?;
                let interest_paid =
                    self.debt
                        .realize_margin_liquidation(repay_asset, repay_credit, aggregate_debt_reduction)?;
                let principal_credit = repay_credit
                    .checked_sub(interest_paid)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let live_debit = aggregate_debt_reduction
                    .checked_sub(principal_credit)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                borrow_position.fixed_base_shares = borrow_position
                    .fixed_base_shares
                    .checked_sub(shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.debt.fixed_base_shares = self
                    .debt
                    .fixed_base_shares
                    .checked_sub(shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.base_side.reserves.live_reserve = self
                    .base_side
                    .reserves
                    .live_reserve
                    .checked_sub(live_debit)
                    .ok_or(ErrorCode::ReserveUnderflow)?;
                self.base_side.reserves.cash_reserve = self
                    .base_side
                    .reserves
                    .cash_reserve
                    .checked_add(principal_credit)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                (interest_paid, debt_reduction)
            }
            MarketAsset::Quote => {
                let shares_to_burn = repayment.shares_to_burn;
                let debt_reduction = repayment.position_debt_reduced;
                let aggregate_debt_reduction =
                    self.debt.fixed_debt_reduction_for_shares(repay_asset, shares_to_burn)?;
                let interest_paid =
                    self.debt
                        .realize_margin_liquidation(repay_asset, repay_credit, aggregate_debt_reduction)?;
                let principal_credit = repay_credit
                    .checked_sub(interest_paid)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let live_debit = aggregate_debt_reduction
                    .checked_sub(principal_credit)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                borrow_position.fixed_quote_shares = borrow_position
                    .fixed_quote_shares
                    .checked_sub(shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.debt.fixed_quote_shares = self
                    .debt
                    .fixed_quote_shares
                    .checked_sub(shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.quote_side.reserves.live_reserve = self
                    .quote_side
                    .reserves
                    .live_reserve
                    .checked_sub(live_debit)
                    .ok_or(ErrorCode::ReserveUnderflow)?;
                self.quote_side.reserves.cash_reserve = self
                    .quote_side
                    .reserves
                    .cash_reserve
                    .checked_add(principal_credit)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                (interest_paid, debt_reduction)
            }
        };
        let debt_delta = -i64::try_from(debt_reduction).map_err(|_| ErrorCode::Overflow)?;
        self.refresh_risk()?;
        let debt_after = match repay_asset {
            MarketAsset::Base => borrow_position.fixed_base_debt(&self.debt)?,
            MarketAsset::Quote => borrow_position.fixed_quote_debt(&self.debt)?,
        };
        let target_contribution = self.debt_capped_global_health_contribution(
            repay_asset,
            debt_after,
            borrow_position.collateral(repay_asset.opposite()),
            &self.risk,
        )?;
        self.reconcile_global_health_contribution(borrow_position, repay_asset, target_contribution)?;
        if debt_after == 0 {
            borrow_position.set_liquidation_cf_bps(repay_asset, 0);
            borrow_position.clear_referral_binding(repay_asset);
        }
        self.reconcile_liquidation_auction(borrow_position)?;
        let market_health = self.market_health()?;
        DebtReceipt::from_market(
            self,
            borrow_position,
            debt_delta,
            repayment.cash_repaid,
            interest_paid,
            &market_health,
        )
    }

    pub fn fixed_repayment_for_max(
        &self,
        borrow_position: &BorrowPosition,
        repay_asset: MarketAsset,
        max_repay_amount: u64,
    ) -> Result<DebtRepaymentQuote> {
        let (position_shares, aggregate_shares, borrow_index_nad) = match repay_asset {
            MarketAsset::Base => (
                borrow_position.fixed_base_shares,
                self.debt.fixed_base_shares,
                self.debt.base_borrow_index_nad,
            ),
            MarketAsset::Quote => (
                borrow_position.fixed_quote_shares,
                self.debt.fixed_quote_shares,
                self.debt.quote_borrow_index_nad,
            ),
        };
        Debt::repayment_for_max(position_shares, aggregate_shares, borrow_index_nad, max_repay_amount)
    }

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

        self.base_side.credit_reserve(receipt.base_reserve_credit, true)?;
        self.quote_side.credit_reserve(receipt.quote_reserve_credit, true)?;
        self.base_side.shares.mint(internal_mint_amount)?;
        self.quote_side.shares.mint(internal_mint_amount)?;
        self.base_side.assert_share_backing()?;
        self.quote_side.assert_share_backing()?;

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

    pub fn swap_reserves(
        &mut self,
        asset_in: MarketAsset,
        amount_in_after_fee: u64,
        amount_out: u64,
        fee_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<SwapReceipt> {
        self.swap_reserves_with_fee_supply(
            asset_in,
            amount_in_after_fee,
            amount_out,
            fee_credit,
            protocol_fee_bps,
            protocol_auction_split,
            None,
        )
    }

    pub fn swap_reserves_with_fee_supply(
        &mut self,
        asset_in: MarketAsset,
        amount_in_after_fee: u64,
        amount_out: u64,
        fee_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        fee_eligible_ylp_supply: Option<u64>,
    ) -> Result<SwapReceipt> {
        let (market_side_in, market_side_out) = self.swap_sides_mut(asset_in);
        require_gte!(
            market_side_out.reserves.cash_reserve,
            amount_out,
            ErrorCode::InsufficientLiquidity
        );

        market_side_in.reserves.live_reserve = market_side_in
            .reserves
            .live_reserve
            .checked_add(amount_in_after_fee)
            .ok_or(ErrorCode::ReserveOverflow)?;
        market_side_in.reserves.cash_reserve = market_side_in
            .reserves
            .cash_reserve
            .checked_add(amount_in_after_fee)
            .ok_or(ErrorCode::ReserveOverflow)?;
        market_side_out.reserves.live_reserve = market_side_out
            .reserves
            .live_reserve
            .checked_sub(amount_out)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        market_side_out.reserves.cash_reserve = market_side_out
            .reserves
            .cash_reserve
            .checked_sub(amount_out)
            .ok_or(ErrorCode::CashReserveUnderflow)?;

        let fees = match fee_eligible_ylp_supply {
            Some(supply) => market_side_in.record_swap_fee_credit_with_supply(
                fee_credit,
                protocol_fee_bps,
                protocol_auction_split,
                supply,
            )?,
            None => market_side_in.record_swap_fee_credit(fee_credit, protocol_fee_bps, protocol_auction_split)?,
        };
        market_side_in.assert_share_backing()?;
        market_side_out.assert_share_backing()?;
        market_side_in.fees.assert_backed()?;

        Ok(SwapReceipt {
            amount_in_after_fee,
            reserve_input_credit: amount_in_after_fee,
            amount_out,
            fee_credit,
            base_fee_credit: fee_credit,
            distributed_surcharge_credit: 0,
            fee_breakdown: SwapFeeBreakdown::default(),
            reserve_in_live_reserve: market_side_in.reserves.live_reserve,
            reserve_out_live_reserve: market_side_out.reserves.live_reserve,
            fees,
        })
    }

    pub fn assert_market_invariants(&self) -> Result<()> {
        self.base_side.assert_share_backing()?;
        self.quote_side.assert_share_backing()?;
        self.base_side.fees.assert_backed()?;
        self.quote_side.fees.assert_backed()?;
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
            .and_then(|value| value.checked_add(hlp_live))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_eq!(
            side.reserves.live_reserve as u128,
            expected_live_reserve,
            ErrorCode::BrokenInvariant
        );
        Ok(())
    }

    pub fn hlp_live_reserve(&self, asset: MarketAsset) -> Result<u128> {
        (self.base_hlp_vault.hlp_live_reserve(asset) as u128)
            .checked_add(self.quote_hlp_vault.hlp_live_reserve(asset) as u128)
            .ok_or(ErrorCode::MarketMathOverflow.into())
    }

    /// Point-in-time lending utilization used by both the IRM and parameter
    /// execution guard. Funding debt belongs to the side whose token was
    /// borrowed, so it is stored on the opposite hLP aggregate vault.
    pub fn lending_utilization_bps(&self, asset: MarketAsset) -> Result<u64> {
        let fixed_debt = match asset {
            MarketAsset::Base => self.debt.fixed_base_debt()?,
            MarketAsset::Quote => self.debt.fixed_quote_debt()?,
        };
        let isolated_debt = self.debt.isolated_debt(asset)?;
        let hlp_funding_debt = match asset {
            MarketAsset::Base => {
                Debt::shares_to_debt(self.quote_hlp_vault.debt_shares, self.debt.base_borrow_index_nad)?
            }
            MarketAsset::Quote => {
                Debt::shares_to_debt(self.base_hlp_vault.debt_shares, self.debt.quote_borrow_index_nad)?
            }
        };
        let total_debt = fixed_debt
            .checked_add(isolated_debt)
            .and_then(|value| value.checked_add(hlp_funding_debt))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        utilization_bps(total_debt, self.side(asset).reserves.cash_reserve as u128)
    }

    pub fn assert_parameter_execution_utilization(&self) -> Result<()> {
        require!(
            self.lending_utilization_bps(MarketAsset::Base)? < PARAMETER_EXECUTION_MAX_UTILIZATION_BPS
                && self.lending_utilization_bps(MarketAsset::Quote)? < PARAMETER_EXECUTION_MAX_UTILIZATION_BPS,
            ErrorCode::UtilizationGuardExceeded
        );
        Ok(())
    }

    pub fn spot_value_in_opposite(&self, asset: MarketAsset, amount: u64) -> Result<u64> {
        require!(amount > 0, ErrorCode::AmountZero);
        let (from_reserve, to_reserve) = match asset {
            MarketAsset::Base => (
                self.base_side.reserves.live_reserve,
                self.quote_side.reserves.live_reserve,
            ),
            MarketAsset::Quote => (
                self.quote_side.reserves.live_reserve,
                self.base_side.reserves.live_reserve,
            ),
        };
        require!(from_reserve > 0 && to_reserve > 0, ErrorCode::InsufficientLiquidity);
        let value = (amount as u128)
            .checked_mul(to_reserve as u128)
            .and_then(|value| value.checked_div(from_reserve as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        u64::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }
}

fn accrue_side(market: &mut Market, asset: MarketAsset, current_slot: u64) -> Result<()> {
    let (index, rate_at_target, last_accrual_slot, fixed_shares, isolated_shares) = match asset {
        MarketAsset::Base => (
            market.debt.base_borrow_index_nad,
            market.debt.base_rate_at_target_nad,
            market.debt.base_last_accrual_slot,
            market.debt.fixed_base_shares,
            market.debt.isolated_base_shares,
        ),
        MarketAsset::Quote => (
            market.debt.quote_borrow_index_nad,
            market.debt.quote_rate_at_target_nad,
            market.debt.quote_last_accrual_slot,
            market.debt.fixed_quote_shares,
            market.debt.isolated_quote_shares,
        ),
    };
    if current_slot <= last_accrual_slot {
        return Ok(());
    }
    let dt_ms = current_slot
        .checked_sub(last_accrual_slot)
        .ok_or(ErrorCode::MarketMathOverflow)?
        .saturating_mul(TARGET_MS_PER_SLOT);

    let hlp_shares = match asset {
        MarketAsset::Base => market.quote_hlp_vault.debt_shares,
        MarketAsset::Quote => market.base_hlp_vault.debt_shares,
    };
    if fixed_shares == 0 && isolated_shares == 0 && hlp_shares == 0 {
        let next_rate_at_target = adapt_rate_at_target_nad(
            rate_at_target,
            -(NAD as i128),
            dt_ms,
            market.config.irm.adjustment_speed_per_year as u128,
            INTEREST_MIN_RATE_AT_TARGET_NAD,
            INTEREST_MAX_RATE_AT_TARGET_NAD,
            INTEREST_MAX_ADAPTATION_STEP_NAD,
        )?;
        match asset {
            MarketAsset::Base => {
                market.debt.base_rate_at_target_nad = next_rate_at_target;
                market.debt.base_last_accrual_slot = current_slot;
            }
            MarketAsset::Quote => {
                market.debt.quote_rate_at_target_nad = next_rate_at_target;
                market.debt.quote_last_accrual_slot = current_slot;
            }
        }
        return Ok(());
    }
    let (cash, live) = match asset {
        MarketAsset::Base => (
            market.base_side.reserves.cash_reserve as u128,
            market.base_side.reserves.live_reserve as u128,
        ),
        MarketAsset::Quote => (
            market.quote_side.reserves.cash_reserve as u128,
            market.quote_side.reserves.live_reserve as u128,
        ),
    };
    let hlp_live = market.hlp_live_reserve(asset)?;
    let cash_backed_before = live
        .checked_sub(cash)
        .and_then(|value| value.checked_sub(hlp_live))
        .ok_or(ErrorCode::BrokenInvariant)?;
    if fixed_shares == 0 && isolated_shares == 0 {
        require_eq!(cash_backed_before, 0, ErrorCode::BrokenInvariant);
    }
    let hlp_debt_before = if hlp_shares == 0 {
        0
    } else {
        Debt::shares_to_debt(hlp_shares, index)?
    };

    // Calculate utilization rates. hLP funding debt counts toward funding cost,
    // but only cash-backed debt accrual grows virtual reserves.
    let debt_before = cash_backed_before
        .checked_add(hlp_debt_before)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let util = utilization_bps(debt_before, cash)?;
    let error = utilization_error_nad(util, market.config.irm.target_utilization_bps as u64)?;
    let rate = instantaneous_rate_apr_nad(rate_at_target, error, market.config.irm.curve_steepness_nad as u128)?;
    let next_index = if index == 0 || dt_ms == 0 || rate == 0 {
        index
    } else {
        let elapsed_ms = dt_ms.min(MAX_INTEREST_ACCRUAL_MS) as u128;
        let growth_nad = rate
            .checked_mul(elapsed_ms)
            .and_then(|value| value.checked_div(MS_PER_YEAR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        if growth_nad == 0 {
            index
        } else {
            let delta = index
                .checked_mul(growth_nad)
                .and_then(|value| value.checked_div(NAD as u128))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            index.checked_add(delta).ok_or(ErrorCode::MarketMathOverflow)?
        }
    };
    let next_rate_at_target = adapt_rate_at_target_nad(
        rate_at_target,
        error,
        dt_ms,
        market.config.irm.adjustment_speed_per_year as u128,
        INTEREST_MIN_RATE_AT_TARGET_NAD,
        INTEREST_MAX_RATE_AT_TARGET_NAD,
        INTEREST_MAX_ADAPTATION_STEP_NAD,
    )?;
    // Fixed and isolated buckets remain separately floored. Combined
    // conversion would manufacture an atom at some index boundaries.
    let fixed_after = if fixed_shares == 0 {
        0
    } else {
        Debt::shares_to_debt(fixed_shares, next_index)?
    };
    let isolated_after = if isolated_shares == 0 {
        0
    } else {
        Debt::shares_to_debt(isolated_shares, next_index)?
    };
    let accrued_interest = fixed_after
        .checked_add(isolated_after)
        .and_then(|after| after.checked_sub(cash_backed_before))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    if accrued_interest > 0 {
        let accrued_interest = u64::try_from(accrued_interest).map_err(|_| ErrorCode::ReserveOverflow)?;
        let side = market.side_mut(asset);
        side.reserves.live_reserve = side
            .reserves
            .live_reserve
            .checked_add(accrued_interest)
            .ok_or(ErrorCode::ReserveOverflow)?;
    }

    match asset {
        MarketAsset::Base => {
            market.debt.base_borrow_index_nad = next_index;
            market.debt.base_rate_at_target_nad = next_rate_at_target;
            market.debt.base_last_accrual_slot = current_slot;
        }
        MarketAsset::Quote => {
            market.debt.quote_borrow_index_nad = next_index;
            market.debt.quote_rate_at_target_nad = next_rate_at_target;
            market.debt.quote_last_accrual_slot = current_slot;
        }
    }
    Ok(())
}

fn total_cash_backed_borrowed(market: &Market, asset: MarketAsset, index_nad: u128) -> Result<u128> {
    let (margin_fixed, isolated) = match asset {
        MarketAsset::Base => (market.debt.fixed_base_shares, market.debt.isolated_base_shares),
        MarketAsset::Quote => (market.debt.fixed_quote_shares, market.debt.isolated_quote_shares),
    };
    let margin_fixed_debt = Debt::shares_to_debt(margin_fixed, index_nad)?;
    let isolated_debt = Debt::shares_to_debt(isolated, index_nad)?;
    margin_fixed_debt
        .checked_add(isolated_debt)
        .ok_or(ErrorCode::MarketMathOverflow.into())
}

fn reconcile_global_health_contribution(
    position_contribution: &mut u64,
    aggregate_contribution: &mut u64,
    target_contribution: u64,
) -> Result<()> {
    match target_contribution.cmp(position_contribution) {
        std::cmp::Ordering::Greater => {
            let delta = target_contribution
                .checked_sub(*position_contribution)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            *aggregate_contribution = aggregate_contribution
                .checked_add(delta)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        }
        std::cmp::Ordering::Less => {
            let delta = position_contribution
                .checked_sub(target_contribution)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            *aggregate_contribution = aggregate_contribution
                .checked_sub(delta)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        }
        std::cmp::Ordering::Equal => {}
    }

    *position_contribution = target_contribution;
    Ok(())
}

fn reserve_for_ylp_mint_ceil(reserve_before: u64, ylp_supply_before: u64, ylp_amount: u64) -> Result<u64> {
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

#[macro_export]
macro_rules! generate_market_seeds {
    ($market:expr) => {
        [
            MARKET_V2_SEED_PREFIX,
            $market.base_side.asset_mint.as_ref(),
            $market.quote_side.asset_mint.as_ref(),
            $market.params_hash.as_ref(),
            &[$market.bump],
        ]
    };
}

#[cfg(test)]
mod tests {
    include!("../../tests/state/market.rs");
}

#[cfg(test)]
mod reserve_tests {
    include!("../../tests/transitions/reserve.rs");
}

#[cfg(test)]
mod interest_tests {
    include!("../../tests/transitions/interest.rs");
}
