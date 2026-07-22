use anchor_lang::prelude::*;

use crate::constants::*;
use crate::errors::ErrorCode;
use crate::math::{
    accrued_index_nad, adapt_rate_at_target_nad, instantaneous_rate_apr_nad, normalize_to_nad, utilization_bps,
    utilization_error_nad,
};
use crate::shared::math::{ceil_div, SqrtU128};
use crate::state::{
    borrow_position::{BorrowPosition, CollateralReceipt},
    futarchy_authority::{FutarchyAuthority, ProtocolAuctionSplit},
};

use super::{Debt, FeesReceipt, HlpVault, MarketAsset, MarketConfig, MarketHealth, MarketSide, Risk};

#[cfg(test)]
use super::Reserves;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarketTimelockAction {
    Scheduled { execute_after_slot: u64 },
    Ready,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DebtReceipt {
    pub debt_delta: i64,
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
    pub amount_out: u64,
    pub fee_credit: u64,
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
        interest_paid: u64,
        health: &MarketHealth,
    ) -> Result<Self> {
        Ok(Self {
            debt_delta,
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

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace)]
pub struct PendingAuthorityChange {
    pub active: bool,
    pub new_authority: Pubkey,
    pub scheduled_by: Pubkey,
    pub scheduled_slot: u64,
    pub execute_after_slot: u64,
}

impl PendingAuthorityChange {
    fn schedule(&mut self, new_authority: Pubkey, signer: Pubkey, current_slot: u64) -> Result<u64> {
        let execute_after_slot = current_slot
            .checked_add(MARKET_GOVERNANCE_DELAY_SLOTS)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.active = true;
        self.new_authority = new_authority;
        self.scheduled_by = signer;
        self.scheduled_slot = current_slot;
        self.execute_after_slot = execute_after_slot;
        Ok(execute_after_slot)
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace)]
pub struct PendingConfigChange {
    pub active: bool,
    pub config: MarketConfig,
    pub scheduled_by: Pubkey,
    pub scheduled_slot: u64,
    pub execute_after_slot: u64,
}

impl PendingConfigChange {
    fn schedule(&mut self, config: MarketConfig, signer: Pubkey, current_slot: u64) -> Result<u64> {
        let execute_after_slot = current_slot
            .checked_add(MARKET_GOVERNANCE_DELAY_SLOTS)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.active = true;
        self.config = config;
        self.scheduled_by = signer;
        self.scheduled_slot = current_slot;
        self.execute_after_slot = execute_after_slot;
        Ok(execute_after_slot)
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

#[account]
#[derive(InitSpace, Default)]
pub struct Market {
    pub version: u8,
    pub ylp_mint: Pubkey,
    pub operator: Pubkey,
    pub manager: Pubkey,
    pub base_side: MarketSide,
    pub quote_side: MarketSide,
    pub config: MarketConfig,
    pub debt: Debt,
    pub base_hlp_vault: HlpVault,
    pub quote_hlp_vault: HlpVault,
    pub risk: Risk,
    pub insurance: Insurance,
    pub pending_config: PendingConfigChange,
    pub pending_operator: PendingAuthorityChange,
    pub pending_manager: PendingAuthorityChange,
    pub params_hash: [u8; 32],
    pub last_update_slot: u64,
    pub reduce_only: bool,
    pub bump: u8,
}

impl Market {
    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        &mut self,
        ylp_mint: Pubkey,
        operator: Pubkey,
        manager: Pubkey,
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
        require_keys_neq!(base_side.asset_mint, quote_side.asset_mint, ErrorCode::InvalidMint);
        require_keys_neq!(operator, Pubkey::default(), ErrorCode::InvalidMarketConfig);
        require_keys_neq!(manager, Pubkey::default(), ErrorCode::InvalidMarketConfig);

        self.version = MARKET_VERSION;
        self.ylp_mint = ylp_mint;
        self.operator = operator;
        self.manager = manager;
        self.base_side = base_side;
        self.quote_side = quote_side;
        self.config = config;
        self.debt = Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            base_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            quote_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            last_accrual_slot: current_slot,
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
        self.pending_config = PendingConfigChange::default();
        self.pending_operator = PendingAuthorityChange::default();
        self.pending_manager = PendingAuthorityChange::default();
        self.params_hash = params_hash;
        self.last_update_slot = current_slot;
        self.reduce_only = false;
        self.bump = bump;
        Ok(())
    }

    pub fn assert_live_with_futarchy(&self, futarchy_authority: &FutarchyAuthority) -> Result<()> {
        self.assert_started()?;
        require!(
            !futarchy_authority.is_reduce_only(self.reduce_only),
            ErrorCode::ReduceOnlyMode
        );
        Ok(())
    }

    pub fn assert_started(&self) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        require!(now >= self.config.start_time, ErrorCode::MarketNotStarted);
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
        let current_slot = Clock::get()?.slot;
        self.accrue_interest_to_slot(current_slot)?;
        if self.base_side.reserves.live_reserve > 0 && self.quote_side.reserves.live_reserve > 0 {
            self.checkpoint_hlp_vaults()?;
            self.refresh_risk()?;
        }
        Ok(())
    }

    pub(crate) fn accrue_interest_to_slot(&mut self, current_slot: u64) -> Result<()> {
        let last = self.debt.last_accrual_slot;
        if current_slot <= last {
            return Ok(());
        }
        let dt_ms = current_slot
            .checked_sub(last)
            .ok_or(ErrorCode::MarketMathOverflow)?
            .saturating_mul(TARGET_MS_PER_SLOT);

        accrue_side(self, MarketAsset::Base, dt_ms)?;
        accrue_side(self, MarketAsset::Quote, dt_ms)?;
        self.debt.last_accrual_slot = current_slot;
        Ok(())
    }

    /// Manager-only authority: sensitive actions (fee setting, risk parameter
    /// changes, and role rotation) require the market manager.
    pub fn assert_manager(&self, signer: Pubkey) -> Result<()> {
        require_keys_eq!(signer, self.manager, ErrorCode::InvalidMarketManager);
        Ok(())
    }

    /// Config authority is manager-only. The operator remains the market's
    /// operational/economic identity, not a config admin.
    pub fn assert_config_authority(&self, signer: Pubkey) -> Result<()> {
        require_keys_eq!(signer, self.manager, ErrorCode::InvalidMarketConfigAuthority);
        Ok(())
    }

    pub fn prepare_config_update(
        &mut self,
        signer: Pubkey,
        config: MarketConfig,
        current_slot: u64,
    ) -> Result<MarketTimelockAction> {
        self.assert_config_authority(signer)?;
        config.validate()?;
        if self.pending_config.active && self.pending_config.config == config {
            require_gte!(
                current_slot,
                self.pending_config.execute_after_slot,
                ErrorCode::GovernanceTimelockNotReady
            );
            return Ok(MarketTimelockAction::Ready);
        }
        require!(config != self.config, ErrorCode::InvalidArgument);
        let execute_after_slot = self.pending_config.schedule(config, signer, current_slot)?;
        Ok(MarketTimelockAction::Scheduled { execute_after_slot })
    }

    pub fn clear_pending_config_update(&mut self) {
        self.pending_config.clear();
    }

    pub fn prepare_operator_update(
        &mut self,
        signer: Pubkey,
        new_operator: Pubkey,
        current_slot: u64,
    ) -> Result<MarketTimelockAction> {
        self.assert_manager(signer)?;
        require_keys_neq!(new_operator, Pubkey::default(), ErrorCode::InvalidArgument);
        require_keys_neq!(new_operator, self.operator, ErrorCode::InvalidArgument);
        if self.pending_operator.active && self.pending_operator.new_authority == new_operator {
            require_gte!(
                current_slot,
                self.pending_operator.execute_after_slot,
                ErrorCode::GovernanceTimelockNotReady
            );
            return Ok(MarketTimelockAction::Ready);
        }
        let execute_after_slot = self.pending_operator.schedule(new_operator, signer, current_slot)?;
        Ok(MarketTimelockAction::Scheduled { execute_after_slot })
    }

    pub fn apply_operator_update(&mut self, new_operator: Pubkey) {
        self.operator = new_operator;
        self.pending_operator.clear();
    }

    pub fn prepare_manager_update(
        &mut self,
        signer: Pubkey,
        new_manager: Pubkey,
        current_slot: u64,
    ) -> Result<MarketTimelockAction> {
        self.assert_manager(signer)?;
        require_keys_neq!(new_manager, Pubkey::default(), ErrorCode::InvalidArgument);
        require_keys_neq!(new_manager, self.manager, ErrorCode::InvalidArgument);
        if self.pending_manager.active && self.pending_manager.new_authority == new_manager {
            require_gte!(
                current_slot,
                self.pending_manager.execute_after_slot,
                ErrorCode::GovernanceTimelockNotReady
            );
            return Ok(MarketTimelockAction::Ready);
        }
        let execute_after_slot = self.pending_manager.schedule(new_manager, signer, current_slot)?;
        Ok(MarketTimelockAction::Scheduled { execute_after_slot })
    }

    pub fn apply_manager_update(&mut self, new_manager: Pubkey) {
        self.manager = new_manager;
        self.pending_manager.clear();
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
            let max_debt = self.buffered_debt_limit_for_liquidation_cf(
                market_asset,
                projected_collateral,
                liquidation_cf_bps,
                &self.risk,
            )?;
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
    ) -> Result<DebtReceipt> {
        require!(borrow_amount > 0, ErrorCode::AmountZero);
        let debt_delta = i64::try_from(borrow_amount).map_err(|_| ErrorCode::Overflow)?;
        if self.risk.k_ema == 0 {
            self.refresh_risk()?;
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
        let daily_limit_slot = self.risk.last_snapshot_slot;
        let daily_borrow_limit = self.daily_limit_for_side(borrow_asset, self.config.max_daily_borrow_bps)?;
        require_borrow_headroom(self.side(borrow_asset), borrow_amount)?;
        self.side_mut(borrow_asset)
            .daily_limits
            .record_borrow(borrow_amount, daily_borrow_limit, daily_limit_slot)?;
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
        DebtReceipt::from_market(self, borrow_position, debt_delta, 0, &market_health)
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
        let debt_before = match repay_asset {
            MarketAsset::Base => borrow_position.fixed_base_debt(&self.debt)?,
            MarketAsset::Quote => borrow_position.fixed_quote_debt(&self.debt)?,
        };
        require_gte!(debt_before, repay_credit as u128, ErrorCode::InsufficientDebt);
        let (interest_paid, debt_reduction) = match repay_asset {
            MarketAsset::Base => {
                let shares_before = borrow_position.fixed_base_shares;
                let shares_to_burn = if repay_credit as u128 == debt_before {
                    shares_before
                } else {
                    Debt::debt_to_shares(repay_credit, self.debt.base_borrow_index_nad)?.min(shares_before)
                };
                let remaining_shares = shares_before
                    .checked_sub(shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let remaining_debt = Debt::shares_to_debt(remaining_shares, self.debt.base_borrow_index_nad)?;
                let debt_reduction = debt_before
                    .checked_sub(remaining_debt)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let debt_reduction = u64::try_from(debt_reduction).map_err(|_| ErrorCode::DebtMathOverflow)?;
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
                let shares_before = borrow_position.fixed_quote_shares;
                let shares_to_burn = if repay_credit as u128 == debt_before {
                    shares_before
                } else {
                    Debt::debt_to_shares(repay_credit, self.debt.quote_borrow_index_nad)?.min(shares_before)
                };
                let remaining_shares = shares_before
                    .checked_sub(shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let remaining_debt = Debt::shares_to_debt(remaining_shares, self.debt.quote_borrow_index_nad)?;
                let debt_reduction = debt_before
                    .checked_sub(remaining_debt)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let debt_reduction = u64::try_from(debt_reduction).map_err(|_| ErrorCode::DebtMathOverflow)?;
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
        DebtReceipt::from_market(self, borrow_position, debt_delta, interest_paid, &market_health)
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
        manager_fee_bps: u16,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<SwapReceipt> {
        self.swap_reserves_with_fee_supply(
            asset_in,
            amount_in_after_fee,
            amount_out,
            fee_credit,
            manager_fee_bps,
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
        manager_fee_bps: u16,
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
                manager_fee_bps,
                protocol_fee_bps,
                protocol_auction_split,
                supply,
            )?,
            None => market_side_in.record_swap_fee_credit(
                fee_credit,
                manager_fee_bps,
                protocol_fee_bps,
                protocol_auction_split,
            )?,
        };
        market_side_in.assert_share_backing()?;
        market_side_out.assert_share_backing()?;
        market_side_in.fees.assert_backed()?;

        Ok(SwapReceipt {
            amount_in_after_fee,
            amount_out,
            fee_credit,
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

fn accrue_side(market: &mut Market, asset: MarketAsset, dt_ms: u64) -> Result<()> {
    let (index, rate_at_target) = match asset {
        MarketAsset::Base => (market.debt.base_borrow_index_nad, market.debt.base_rate_at_target_nad),
        MarketAsset::Quote => (market.debt.quote_borrow_index_nad, market.debt.quote_rate_at_target_nad),
    };
    let cash = match asset {
        MarketAsset::Base => market.base_side.reserves.cash_reserve,
        MarketAsset::Quote => market.quote_side.reserves.cash_reserve,
    } as u128;

    // Calculate utilization rates. hLP funding debt counts toward funding cost,
    // but only cash-backed debt accrual grows virtual reserves.
    let debt_before = total_borrowed(market, asset, index)?;
    let util = utilization_bps(debt_before, cash)?;
    let error = utilization_error_nad(util, INTEREST_TARGET_UTILIZATION_BPS)?;
    let rate = instantaneous_rate_apr_nad(rate_at_target, error, INTEREST_CURVE_STEEPNESS_NAD)?;
    let next_index = accrued_index_nad(index, rate, dt_ms)?;
    let next_rate_at_target = adapt_rate_at_target_nad(
        rate_at_target,
        error,
        dt_ms,
        INTEREST_ADJUSTMENT_SPEED_PER_YEAR,
        INTEREST_MIN_RATE_AT_TARGET_NAD,
        INTEREST_MAX_RATE_AT_TARGET_NAD,
        INTEREST_MAX_ADAPTATION_STEP_NAD,
    )?;
    let cash_backed_before = total_cash_backed_borrowed(market, asset, index)?;
    let cash_backed_after = total_cash_backed_borrowed(market, asset, next_index)?;
    let accrued_interest = cash_backed_after
        .checked_sub(cash_backed_before)
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
        }
        MarketAsset::Quote => {
            market.debt.quote_borrow_index_nad = next_index;
            market.debt.quote_rate_at_target_nad = next_rate_at_target;
        }
    }
    Ok(())
}

fn total_borrowed(market: &Market, asset: MarketAsset, index_nad: u128) -> Result<u128> {
    total_cash_backed_borrowed(market, asset, index_nad)?
        .checked_add(total_hlp_funding_debt(market, asset, index_nad)?)
        .ok_or(ErrorCode::MarketMathOverflow.into())
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

fn total_hlp_funding_debt(market: &Market, asset: MarketAsset, index_nad: u128) -> Result<u128> {
    let hlp_shares = match asset {
        MarketAsset::Base => market.quote_hlp_vault.debt_shares,
        MarketAsset::Quote => market.base_hlp_vault.debt_shares,
    };
    Debt::shares_to_debt(hlp_shares, index_nad)
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

fn require_borrow_headroom(debt_side: &MarketSide, borrow_amount: u64) -> Result<()> {
    require_gte!(
        debt_side.reserves.cash_reserve,
        borrow_amount,
        ErrorCode::InsufficientBorrowHeadroom
    );
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
