use super::*;
use crate::market::SwapFeeBreakdown;
use proptest::prelude::*;

impl<'a> NewPositionPreviewContext<'a> {
    fn new(market: &'a Market, debt_asset: MarketAsset, collateral_amount: u64, risk: &'a Risk) -> Result<Self> {
        let collateral_asset = debt_asset.opposite();
        let (collateral_virtual_reserve_nad, debt_virtual_reserve_nad) =
            market.pessimistic_virtual_reserves_nad(collateral_asset, risk, true)?;
        Ok(Self {
            market,
            debt_asset,
            collateral_amount,
            risk,
            existing_total_debt_nad: market.total_fixed_debt_nad(debt_asset)?,
            current_aggregate_contribution: match debt_asset {
                MarketAsset::Base => market.debt.global_health_quote_contribution_for_base_debt,
                MarketAsset::Quote => market.debt.global_health_base_contribution_for_quote_debt,
            },
            collateral_amount_nad: normalize_to_nad(
                collateral_amount as u128,
                market.side(collateral_asset).asset_decimals,
            )?,
            collateral_virtual_reserve_nad,
            debt_virtual_reserve_nad,
        })
    }

    fn is_accepted(&self, projected_debt_amount: u64) -> Result<bool> {
        let (terms, _) = self.terms(projected_debt_amount)?;
        Ok(terms.max_debt >= projected_debt_amount
            && terms.projected_market_health_bps >= self.market.config.borrow_market_health_floor_bps as u64)
    }
}

fn max_new_position_debt_by_dynamic_health(context: &NewPositionPreviewContext<'_>, upper_bound: u64) -> Result<u64> {
    let current_health = context.market.market_health_from_risk(context.risk)?;
    if context.market.assert_market_health_snapshot(&current_health).is_err() {
        return Ok(0);
    }

    let mut low = 0_u64;
    let mut high = upper_bound;
    while low < high {
        let midpoint = low + (high - low) / 2 + 1;
        let (terms, _) = context.terms(midpoint)?;
        if terms.max_debt >= midpoint
            && terms.projected_market_health_bps >= context.market.config.borrow_market_health_floor_bps as u64
        {
            low = midpoint;
        } else {
            high = midpoint - 1;
        }
    }
    Ok(low)
}

fn preview_test_market(existing_base_debt: u64, aggregate_quote_contribution: u64) -> Market {
    let mut market = Market::default();
    market.base_side.asset_decimals = 0;
    market.quote_side.asset_decimals = 0;
    market.base_side.reserves.live_reserve = 1_000_000;
    market.base_side.reserves.cash_reserve = 1_000_000;
    market.quote_side.reserves.live_reserve = 1_000_000;
    market.quote_side.reserves.cash_reserve = 1_000_000;
    market.debt.base_borrow_index_nad = NAD as u128;
    market.debt.quote_borrow_index_nad = NAD as u128;
    market.debt.fixed_base_shares = existing_base_debt as u128;
    market.debt.global_health_quote_contribution_for_base_debt = aggregate_quote_contribution;
    market.config.global_health_contribution_cap_bps = 15_000;
    market.config.borrow_market_health_floor_bps = 11_000;
    market.risk = Risk {
        base_price_ema_nad: NAD,
        quote_price_ema_nad: NAD,
        directional_base_price_ema_nad: NAD,
        directional_quote_price_ema_nad: NAD,
        q_ema_nad: 1_000_000_u128 * NAD as u128,
        ..Risk::default()
    };
    market
}

#[test]
fn dynamic_health_binary_search_matches_brute_force() {
    let market = preview_test_market(50_000, 75_000);
    let upper_bound = 5_000;
    let context = NewPositionPreviewContext::new(&market, MarketAsset::Base, 5_000, &market.risk).unwrap();
    let binary = max_new_position_debt_by_dynamic_health(&context, upper_bound).unwrap();
    let brute = (0..=upper_bound)
        .filter(|candidate| context.is_accepted(*candidate).unwrap())
        .max()
        .unwrap();

    assert_eq!(binary, brute);
}

#[test]
fn reserve_custodied_fee_split_does_not_apply_a_second_transfer_fee() {
    let fee = SwapFeeBreakdown {
        base_fee_debit: 60,
        distributed_surcharge_debit: 40,
        claimable_fee_debit: 100,
        retained_surcharge: 25,
        ..SwapFeeBreakdown::default()
    };

    let (base_credit, distributed_credit) = split_claimable_fee_credit(&fee, 100).unwrap();

    assert_eq!(base_credit, 60);
    assert_eq!(distributed_credit, 40);
    assert_eq!(base_credit + distributed_credit, fee.claimable_fee_debit);
}

#[test]
fn preview_and_execution_use_identical_state_plans() {
    let mut preview_market = preview_test_market(0, 0);
    preview_market.base_side.shares.ylp_supply = 1_000_000;
    preview_market.quote_side.shares.ylp_supply = 1_000_000;
    let mut execution_market = preview_market.clone();
    let context = SwapContext {
        current_slot: 7,
        asset_in: MarketAsset::Base,
        reserve_credit: 50_000,
        reserved_daily_borrow: 0,
    };

    let preview = context.plan(&mut preview_market).unwrap();
    let execution = context.plan(&mut execution_market).unwrap();

    assert_eq!(preview.quote, execution.quote);
    assert_eq!(preview.base_pre_rebalance, execution.base_pre_rebalance);
    assert_eq!(preview.quote_pre_rebalance, execution.quote_pre_rebalance);
    assert_eq!(preview.fee_eligible_ylp_supply, execution.fee_eligible_ylp_supply);
    assert_eq!(preview_market.amm, execution_market.amm);
}

#[test]
fn preview_daily_remaining_uses_the_authoritative_bucket_decay() {
    let mut market = preview_test_market(0, 0);
    market.config.max_daily_borrow_bps = 2_000;
    let limit = market
        .daily_limit_for_side(MarketAsset::Base, market.config.max_daily_borrow_bps)
        .unwrap();
    market.record_new_borrow(MarketAsset::Base, limit / 2, 0).unwrap();
    let slot = crate::constants::MS_PER_DAY / crate::constants::TARGET_MS_PER_SLOT / 4;

    assert_eq!(
        daily_borrow_remaining(&market, MarketAsset::Base, slot).unwrap(),
        market.daily_borrow_budget(MarketAsset::Base, slot).unwrap().1
    );
}

#[test]
fn concentrated_swap_preview_and_execution_reject_the_same_stale_hlp_path() {
    let mut market = preview_test_market(0, 0);
    market.base_side.shares.ylp_supply = 1_000_000;
    market.quote_side.shares.ylp_supply = 1_000_000;
    market.config.settlement_divergence_bps = 1;
    market.config.target_hlp_leverage_bps = 20_000;
    market.config.max_daily_borrow_bps = crate::state::MAX_DAILY_BORROW_BPS;
    market.config.amm.peak_depth_nad = 200 * NAD;
    market.config.amm.fade_scale_nad = NAD / 10;
    market.checkpoint_amm_neutral_inventory(1).unwrap();
    market
        .deposit_single_sided(MarketAsset::Base, 100_000, 1, 0)
        .unwrap();

    // Create genuine stale inventory without running hLP correction, then
    // checkpoint the resulting actionable exposure. The settlement
    // reference remains the last actual hedge settlement price.
    let stale_trade = market.quote_curve_exact_in(MarketAsset::Base, 150_000, 1).unwrap();
    market
        .swap_reserves(
            MarketAsset::Base,
            150_000,
            stale_trade.amount_out,
            0,
            0,
            crate::state::ProtocolAuctionSplit::default(),
        )
        .unwrap();
    market.checkpoint_amm_neutral_inventory(1).unwrap();
    market.checkpoint_hlp_vaults().unwrap();
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);

    let context = SwapContext {
        current_slot: 1,
        asset_in: MarketAsset::Base,
        reserve_credit: 50_000,
        reserved_daily_borrow: 0,
    };
    let mut preview_market = market.clone();
    let preview_error = context.plan(&mut preview_market).unwrap_err();
    let mut execution_market = market.clone();
    let execution_error = context.plan(&mut execution_market).unwrap_err();

    assert_eq!(preview_error, error!(ErrorCode::HlpSettlementUnavailable));
    assert_eq!(execution_error, preview_error);
}

proptest! {
    #[test]
    fn dynamic_health_acceptance_is_monotonic(
        existing_debt in 0_u64..100_000,
        existing_contribution_bps in 13_000_u64..=15_000,
        collateral_amount in 1_u64..500_000,
        lower_candidate in 0_u64..300_000,
        candidate_delta in 0_u64..300_000,
    ) {
        let aggregate_contribution = existing_debt
            .saturating_mul(existing_contribution_bps)
            / BPS_DENOMINATOR as u64;
        let market = preview_test_market(existing_debt, aggregate_contribution);
        let context = NewPositionPreviewContext::new(
            &market,
            MarketAsset::Base,
            collateral_amount,
            &market.risk,
        )
        .unwrap();
        let higher_candidate = lower_candidate.saturating_add(candidate_delta);
        let lower_accepted = context.is_accepted(lower_candidate).unwrap();
        let higher_accepted = context.is_accepted(higher_candidate).unwrap();

        let (cached_terms, cached_contribution) = context.terms(lower_candidate).unwrap();
        let projected_debt_nad = normalize_to_nad(lower_candidate as u128, 0).unwrap();
        let projected_aggregate = aggregate_contribution
            .checked_add(cached_contribution)
            .unwrap();
        let full_terms = market
            .dynamic_borrow_terms(
                MarketAsset::Base,
                collateral_amount,
                existing_debt as u128 * NAD as u128,
                existing_debt as u128 * NAD as u128 + projected_debt_nad,
                projected_aggregate,
                &market.risk,
            )
            .unwrap();

        prop_assert!(!higher_accepted || lower_accepted);
        prop_assert_eq!(cached_terms, full_terms);

        let upper_bound = 600_000;
        let maximum = max_new_position_debt_by_dynamic_health(&context, upper_bound).unwrap();
        if maximum > 0 {
            prop_assert!(context.is_accepted(maximum).unwrap());
        }
        if maximum < upper_bound {
            prop_assert!(!context.is_accepted(maximum + 1).unwrap());
        }
    }
}
