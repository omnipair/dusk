use crate::{constants::NAD, errors::ErrorCode, state::*};
use anchor_lang::prelude::*;

impl GovernanceControl {
    pub fn initialize_baseline(&mut self, market: &Market) {
        if !self.baseline_initialized {
            self.baseline_config = market.config;
            self.baseline_insurance_principal_coverage_bps = market.insurance.principal_coverage_bps;
            self.baseline_insurance_per_day_bps = market.insurance.per_day_draw_bps;
            self.baseline_tranche_policy = market.tranche_policy;
            self.baseline_junior_request_fee_bps = market.junior_exit_config.request_fee_bps;
            self.baseline_initialized = true;
        }
    }

    pub fn reanchor(&mut self, update: &MarketParameterUpdate) {
        if !self.baseline_initialized {
            return;
        }
        match update {
            MarketParameterUpdate::Fee(profile) => self.baseline_config.copy_fee_profile_fields(*profile),
            MarketParameterUpdate::Concentration {
                peak_amplification_nad,
                core_half_width_bps,
                fade_width_bps,
            } => {
                self.baseline_config.amm.peak_amplification_nad = *peak_amplification_nad;
                self.baseline_config.amm.core_half_width_bps = *core_half_width_bps;
                self.baseline_config.amm.fade_width_bps = *fade_width_bps;
            }
            MarketParameterUpdate::Irm(irm) => self.baseline_config.irm = *irm,
            MarketParameterUpdate::EmaHalfLives {
                price_ms,
                directional_price_ms,
                curve_depth_ms,
                center_price_ms,
            } => {
                self.baseline_config.ema_half_life_ms = *price_ms;
                self.baseline_config.directional_ema_half_life_ms = *directional_price_ms;
                self.baseline_config.curve_depth_ema_half_life_ms = *curve_depth_ms;
                self.baseline_config.amm.center_ema_half_life_ms = *center_price_ms;
            }
            MarketParameterUpdate::DailyBorrowLimit { max_daily_borrow_bps } => {
                self.baseline_config.max_daily_borrow_bps = *max_daily_borrow_bps
            }
            MarketParameterUpdate::CenterController {
                adjustment_threshold_nad,
                adjustment_step_nad,
                min_adjustment_interval_slots,
            } => {
                self.baseline_config.amm.adjustment_threshold_nad = *adjustment_threshold_nad;
                self.baseline_config.amm.adjustment_step_nad = *adjustment_step_nad;
                self.baseline_config.amm.min_adjustment_interval_slots = *min_adjustment_interval_slots;
            }
            MarketParameterUpdate::InsuranceDrawCaps {
                principal_coverage_bps,
                per_day_bps,
            } => {
                self.baseline_insurance_principal_coverage_bps = *principal_coverage_bps;
                self.baseline_insurance_per_day_bps = *per_day_bps;
            }
            MarketParameterUpdate::Tranche {
                junior_income_share_bps,
                request_fee_bps,
                liquidation_fee_bps,
            } => {
                self.baseline_tranche_policy = TranchePolicy {
                    junior_income_share_bps: *junior_income_share_bps,
                    liquidation_fee_bps: *liquidation_fee_bps,
                };
                self.baseline_junior_request_fee_bps = *request_fee_bps;
            }
            MarketParameterUpdate::Oracle(_) => {}
        }
    }

    pub fn validate_manager_updates(&self, market: &Market, updates: &[MarketParameterUpdate]) -> Result<()> {
        require!(self.baseline_initialized, ErrorCode::InvalidManager);
        market.validate_parameter_updates(updates)?;
        for update in updates {
            match update {
                MarketParameterUpdate::Fee(next) => {
                    let old = market.config.fee_profile();
                    let baseline = self.baseline_config.fee_profile();
                    macro_rules! fee_ceiling {
                        ($field:ident, $ceiling:expr) => {
                            if next.$field != old.$field {
                                require!(next.$field <= $ceiling, ErrorCode::ManagerPolicyExceeded);
                            }
                        };
                    }
                    fee_ceiling!(base_fee_bps, 100);
                    fee_ceiling!(launch_fee_start_bps, 100);
                    fee_ceiling!(launch_rate_limit_max_fee_bps, 100);
                    fee_ceiling!(compounding_fee_bps, 10_000);
                    // Activation must obey the manager ceiling even when an
                    // LP left a larger, previously inactive cap unchanged.
                    if old.divergence_fee_coefficient_nad == 0 && next.divergence_fee_coefficient_nad > 0 {
                        require!(
                            next.divergence_fee_share_cap_bps <= 250,
                            ErrorCode::ManagerPolicyExceeded
                        );
                    }
                    if old.volatility_fee_coefficient_nad == 0 && next.volatility_fee_coefficient_nad > 0 {
                        require!(
                            next.volatility_fee_share_cap_bps <= 250,
                            ErrorCode::ManagerPolicyExceeded
                        );
                    }
                    for (value, previous) in [
                        (next.divergence_fee_share_cap_bps, old.divergence_fee_share_cap_bps),
                        (next.volatility_fee_share_cap_bps, old.volatility_fee_share_cap_bps),
                    ] {
                        if value != previous {
                            require!(value <= 250, ErrorCode::ManagerPolicyExceeded);
                        }
                    }
                    macro_rules! relative_fee {
                        ($($field:ident),+ $(,)?) => {$(
                            // Optional components can be disabled, or activated
                            // from a zero LP reference, within protocol bounds.
                            if next.$field != 0 && baseline.$field != 0 {
                                relative_update(next.$field.into(), old.$field.into(), baseline.$field.into(), 2_000)?;
                            }
                        )+};
                    }
                    relative_fee!(
                        divergence_fee_coefficient_nad,
                        volatility_fee_coefficient_nad,
                        volatility_half_life_ms,
                        volatility_shock_cap_nad,
                        volatility_accumulator_cap_nad,
                        launch_market_price_step_bps,
                        launch_market_number_of_periods,
                        launch_market_reduction_factor_bps,
                        launch_rate_limit_reference_nad,
                        launch_rate_limit_increment_bps
                    );
                    for (value, previous) in [
                        (next.launch_fee_duration_seconds, old.launch_fee_duration_seconds),
                        (
                            next.launch_rate_limit_duration_seconds,
                            old.launch_rate_limit_duration_seconds,
                        ),
                    ] {
                        if value != previous {
                            require!(value <= 30 * 86_400, ErrorCode::ManagerPolicyExceeded);
                        }
                    }
                    // Valid fee-asset and launch modes are permitted. All launch
                    // fields are independently frozen once trading has started.
                }
                MarketParameterUpdate::Concentration {
                    peak_amplification_nad,
                    core_half_width_bps,
                    fade_width_bps,
                } => {
                    if *peak_amplification_nad == NAD && *core_half_width_bps == 0 && *fade_width_bps == 0 {
                        continue;
                    }
                    anchored_band(
                        *peak_amplification_nad,
                        self.baseline_config.amm.peak_amplification_nad,
                        2_500,
                    )?;
                    require!(
                        u64::from(*core_half_width_bps)
                            .abs_diff(u64::from(self.baseline_config.amm.core_half_width_bps))
                            <= 500
                            && u64::from(*fade_width_bps).abs_diff(u64::from(self.baseline_config.amm.fade_width_bps))
                                <= 500,
                        ErrorCode::ManagerPolicyExceeded
                    );
                }
                MarketParameterUpdate::Irm(next) => {
                    let old = market.config.irm;
                    let baseline = self.baseline_config.irm;
                    relative_update(
                        next.lower_signal_nad,
                        old.lower_signal_nad,
                        baseline.lower_signal_nad,
                        2_500,
                    )?;
                    relative_update(
                        next.upper_signal_nad,
                        old.upper_signal_nad,
                        baseline.upper_signal_nad,
                        2_500,
                    )?;
                    if next.half_life_ms != old.half_life_ms {
                        half_to_double(next.half_life_ms, baseline.half_life_ms)?;
                    }
                    absolute_update(
                        next.initial_apr_nad,
                        old.initial_apr_nad,
                        baseline.initial_apr_nad,
                        2 * NAD / 100,
                    )?;
                    absolute_update(
                        next.minimum_apr_nad,
                        old.minimum_apr_nad,
                        baseline.minimum_apr_nad,
                        2 * NAD / 100,
                    )?;
                    if next.maximum_apr_nad != old.maximum_apr_nad && next.maximum_apr_nad != 0 {
                        if baseline.maximum_apr_nad == 0 {
                            require!(
                                next.maximum_apr_nad >= next.initial_apr_nad
                                    && next.maximum_apr_nad as u128
                                        <= baseline.initial_apr_nad as u128 + (NAD / 10) as u128,
                                ErrorCode::ManagerPolicyExceeded
                            );
                        } else {
                            require!(
                                next.maximum_apr_nad.abs_diff(baseline.maximum_apr_nad) <= NAD / 10,
                                ErrorCode::ManagerPolicyExceeded
                            );
                        }
                    }
                    if next.borrow_utilization_limit_bps != old.borrow_utilization_limit_bps {
                        require!(
                            next.borrow_utilization_limit_bps <= 8_000,
                            ErrorCode::ManagerPolicyExceeded
                        );
                    }
                }
                MarketParameterUpdate::EmaHalfLives {
                    price_ms,
                    directional_price_ms,
                    curve_depth_ms,
                    center_price_ms,
                } => {
                    for (value, previous, baseline) in [
                        (
                            *price_ms,
                            market.config.ema_half_life_ms,
                            self.baseline_config.ema_half_life_ms,
                        ),
                        (
                            *directional_price_ms,
                            market.config.directional_ema_half_life_ms,
                            self.baseline_config.directional_ema_half_life_ms,
                        ),
                        (
                            *curve_depth_ms,
                            market.config.curve_depth_ema_half_life_ms,
                            self.baseline_config.curve_depth_ema_half_life_ms,
                        ),
                        (
                            *center_price_ms,
                            market.config.amm.center_ema_half_life_ms,
                            self.baseline_config.amm.center_ema_half_life_ms,
                        ),
                    ] {
                        if value != previous {
                            half_to_double(value, baseline)?;
                        }
                    }
                }
                MarketParameterUpdate::DailyBorrowLimit { max_daily_borrow_bps } => {
                    if *max_daily_borrow_bps != market.config.max_daily_borrow_bps {
                        require!(
                            *max_daily_borrow_bps <= self.baseline_config.max_daily_borrow_bps,
                            ErrorCode::ManagerPolicyExceeded
                        );
                    }
                }
                MarketParameterUpdate::CenterController {
                    adjustment_threshold_nad,
                    adjustment_step_nad,
                    min_adjustment_interval_slots,
                } => {
                    // A manager may tune the controller but cannot turn it off.
                    // Full config validation preserves threshold >= step and the
                    // protocol's adjustment and interval hard ceilings.
                    require!(
                        *adjustment_step_nad > 0
                            && *adjustment_step_nad <= NAD / 1_000
                            && *adjustment_threshold_nad <= NAD / 100
                            && *min_adjustment_interval_slots >= 3_600_000 / crate::constants::TARGET_MS_PER_SLOT,
                        ErrorCode::ManagerPolicyExceeded
                    );
                }
                MarketParameterUpdate::InsuranceDrawCaps {
                    principal_coverage_bps,
                    per_day_bps,
                } => {
                    absolute_update(
                        u64::from(*principal_coverage_bps),
                        u64::from(market.insurance.principal_coverage_bps),
                        u64::from(self.baseline_insurance_principal_coverage_bps),
                        500,
                    )?;
                    absolute_update(
                        u64::from(*per_day_bps),
                        u64::from(market.insurance.per_day_draw_bps),
                        u64::from(self.baseline_insurance_per_day_bps),
                        500,
                    )?;
                }
                MarketParameterUpdate::Tranche {
                    junior_income_share_bps,
                    request_fee_bps,
                    liquidation_fee_bps,
                } => {
                    absolute_update(
                        u64::from(*junior_income_share_bps),
                        u64::from(market.tranche_policy.junior_income_share_bps),
                        u64::from(self.baseline_tranche_policy.junior_income_share_bps),
                        1_000,
                    )?;
                    absolute_update(
                        u64::from(*request_fee_bps),
                        u64::from(market.junior_exit_config.request_fee_bps),
                        u64::from(self.baseline_junior_request_fee_bps),
                        5,
                    )?;
                    absolute_update(
                        u64::from(*liquidation_fee_bps),
                        u64::from(market.tranche_policy.liquidation_fee_bps),
                        u64::from(self.baseline_tranche_policy.liquidation_fee_bps),
                        25,
                    )?;
                }
                MarketParameterUpdate::Oracle(_) => return err!(ErrorCode::ManagerPolicyExceeded),
            }
        }
        Ok(())
    }
}

fn anchored_band(value: u64, baseline: u64, allowance_bps: u16) -> Result<()> {
    require!(
        (value.abs_diff(baseline) as u128) * 10_000 <= (baseline as u128) * u128::from(allowance_bps),
        ErrorCode::ManagerPolicyExceeded
    );
    Ok(())
}
fn relative_update(value: u64, previous: u64, baseline: u64, band_bps: u16) -> Result<()> {
    if value == previous {
        return Ok(());
    }
    anchored_band(value, baseline, band_bps)?;

    Ok(())
}
fn absolute_update(value: u64, previous: u64, baseline: u64, band: u64) -> Result<()> {
    if value != previous {
        require!(value.abs_diff(baseline) <= band, ErrorCode::ManagerPolicyExceeded);
    }
    Ok(())
}
fn half_to_double(value: u64, baseline: u64) -> Result<()> {
    require!(
        (value as u128) * 2 >= baseline as u128 && value as u128 <= (baseline as u128) * 2,
        ErrorCode::ManagerPolicyExceeded
    );
    Ok(())
}
