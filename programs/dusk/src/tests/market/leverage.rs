use super::*;
use crate::{
    constants::{INTEREST_INITIAL_RATE_AT_TARGET_NAD, MARKET_LAYOUT_VERSION, MIN_HALF_LIFE_MS, NAD},
    instructions::{leverage_entry_limit_satisfied, leverage_entry_price_nad, SwapRequest},
    market::liquidity::SwapCashPolicy,
    math::ConcentratedCurveParameters,
    state::{
        AmmConfig, AmmState, Debt, HlpVault, Insurance, MarketConfig, MarketSide, ProtocolAuctionSplit, ReserveShares,
        Reserves, Risk,
    },
};

fn test_market(base_cash: u64, quote_cash: u64) -> Market {
    let mut base_side = MarketSide {
        reserves: Reserves {
            live_reserve: base_cash,
            cash_reserve: base_cash,
            ..Reserves::default()
        },
        ..MarketSide::default()
    };
    let mut quote_side = MarketSide {
        reserves: Reserves {
            live_reserve: quote_cash,
            cash_reserve: quote_cash,
            ..Reserves::default()
        },
        ..MarketSide::default()
    };
    let ylp_supply = base_cash.min(quote_cash).max(1);
    base_side.shares = ReserveShares { ylp_supply };
    quote_side.shares = ReserveShares { ylp_supply };
    let mut market = Market {
        version: MARKET_LAYOUT_VERSION,
        ylp_mint: Pubkey::new_unique(),
        base_side,
        quote_side,
        config: MarketConfig {
            swap_fee_bps: 0,
            divergence_fee_share_cap_bps: 2_000,
            volatility_fee_share_cap_bps: 2_000,
            max_daily_borrow_bps: 3_000,
            ..MarketConfig::default()
        },
        amm: Default::default(),
        debt: Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            base_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            quote_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            ..Debt::default()
        },
        base_hlp_vault: HlpVault::default(),
        quote_hlp_vault: HlpVault::default(),
        risk: Risk::default(),
        insurance: Insurance::default(),
        params_hash: [0u8; 32],
        initial_liquidity_authority: Pubkey::default(),
        governance_locked_ylp: 0,
        parameter_revisions: [0; 7],
        last_marginal_observation_nad: 0,
        curve_revision: 0,
        risk_revision: 0,
        last_update_slot: 0,
        reduce_only: false,
        bump: 255,
    };
    market.prepare_amm_for_swap(0).unwrap();
    market.refresh_risk().unwrap();
    market
}

fn empty_position() -> LeveragePosition {
    LeveragePosition {
        owner: Pubkey::default(),
        market: Pubkey::default(),
        position_id: Pubkey::default(),
        referral_partner: Pubkey::default(),
        referral_interest_share_bps: 0,
        debt_asset: 0,
        collateral_amount: 0,
        margin_amount: 0,
        open_notional: 0,
        debt_principal: 0,
        debt_shares: 0,
        multiplier_bps: 0,
        opened_at: 0,
        opened_slot: 0,
        bump: 0,
    }
}

#[test]
fn leverage_entry_price_is_side_aware_and_conservatively_rounded() {
    let mut market = test_market(1_000_000, 2_000_000);

    assert_eq!(
        leverage_entry_price_nad(&market, MarketAsset::Quote, 200, 100).unwrap(),
        2 * NAD
    );
    assert_eq!(
        leverage_entry_price_nad(&market, MarketAsset::Base, 100, 200).unwrap(),
        2 * NAD
    );
    assert_eq!(
        leverage_entry_price_nad(&market, MarketAsset::Quote, 201, 100).unwrap(),
        2_010_000_000
    );
    assert_eq!(
        leverage_entry_price_nad(&market, MarketAsset::Base, 100, 199).unwrap(),
        1_990_000_000
    );
    assert!(leverage_entry_limit_satisfied(
        MarketAsset::Quote,
        1_999_999_999,
        2 * NAD,
    ));
    assert!(!leverage_entry_limit_satisfied(
        MarketAsset::Quote,
        2_000_000_001,
        2 * NAD,
    ));
    assert!(leverage_entry_limit_satisfied(
        MarketAsset::Base,
        2_000_000_001,
        2 * NAD,
    ));
    assert!(!leverage_entry_limit_satisfied(
        MarketAsset::Base,
        1_999_999_999,
        2 * NAD,
    ));

    market.base_side.asset_decimals = 10;
    market.quote_side.asset_decimals = 9;
    assert_eq!(
        leverage_entry_price_nad(&market, MarketAsset::Quote, 1, 11).unwrap(),
        909_090_910
    );
    assert_eq!(
        leverage_entry_price_nad(&market, MarketAsset::Base, 11, 1).unwrap(),
        909_090_909
    );
}

fn seeded_position(
    market: &mut Market,
    debt_asset: MarketAsset,
    debt_amount: u64,
    collateral_amount: u64,
) -> LeveragePosition {
    let debt_shares = market.debt.add_isolated_debt(debt_asset, debt_amount).unwrap();
    match debt_asset {
        MarketAsset::Base => {
            market.base_side.reserves.cash_reserve -= debt_amount;
        }
        MarketAsset::Quote => {
            market.quote_side.reserves.cash_reserve -= debt_amount;
        }
    }
    let mut position = empty_position();
    position.initialize(
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::default(),
        0,
        debt_asset,
        collateral_amount,
        debt_amount,
        debt_amount * 2,
        debt_amount,
        debt_shares,
        20_000,
        0,
        0,
        255,
    );
    position
}

fn full_fee_credit(quote: &LeverageSwapQuote) -> LeverageSwapFeeCredit {
    LeverageSwapFeeCredit::from_total_actual_credit(quote, quote.fee_credit).unwrap()
}

fn prepared_leverage_swap(
    market: &Market,
    swap: LeverageSwapQuote,
    cash_policy: SwapCashPolicy,
) -> PreparedLeverageSwap {
    let asset_in = MarketAsset::try_from_code(swap.asset_in).unwrap();
    let pre_state = market.dynamic_fee_pre_state(swap.quoted_slot).unwrap();
    let preliminary = market
        .preliminary_swap_inputs_for_state(asset_in, swap.amount_in, swap.quoted_slot, pre_state)
        .unwrap();
    let integrated = market
        .quote_concentrated_integrated_with_fee(asset_in, swap.amount_in, preliminary, 0)
        .unwrap()
        .unwrap();
    let concentrated_transition = prepare_concentrated_hlp_transition(market, integrated, asset_in).unwrap();
    PreparedLeverageSwap {
        concentrated_transition: Some(Box::new(concentrated_transition)),
        swap,
        base_pre_rebalance: HlpRebalanceReceipt::default(),
        quote_pre_rebalance: HlpRebalanceReceipt {
            target_asset: MarketAsset::Quote,
            ..HlpRebalanceReceipt::default()
        },
        fee_eligible_ylp_supply: market
            .side(MarketAsset::try_from_code(swap.asset_in).unwrap())
            .shares
            .ylp_supply,
        interest_eligibility: HlpYieldEligibility {
            ylp_supply: market.base_side.shares.ylp_supply,
            base_hlp_ylp_shares: market.base_hlp_vault.ylp_shares,
            quote_hlp_ylp_shares: market.quote_hlp_vault.ylp_shares,
        },
        cash_policy,
    }
}

/// Independent mutation oracle copied from the pre-plan lifecycle. Keep this
/// deliberately imperative: production derives a fixed successor first and
/// commits it only after preflight, while this reference exercises the old
/// Base/Quote reserve and debt mutations in their original order.
fn apply_leverage_lifecycle_transition_reference(
    market: &mut Market,
    policy: SwapCashPolicy,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
    amount_out: u64,
) -> Result<LeverageLifecycleTransition> {
    let mut transition = LeverageLifecycleTransition::default();
    let mut cash_debit_out = amount_out;
    let mut extra_live_debit_out = 0_u64;
    let mut debt_curve_reserve_before_share_removal = None;

    match policy {
        SwapCashPolicy::Spot => {}
        SwapCashPolicy::Borrow { asset, amount } => {
            require!(asset == asset_in, ErrorCode::BrokenInvariant);
            market.debit_leverage_cash(asset, amount)?;
        }
        SwapCashPolicy::Decrease {
            debt_asset,
            debt_shares,
            debt_principal,
        } => {
            require!(debt_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
            let mut shares = debt_shares;
            let mut principal = debt_principal;
            transition.clearance =
                market
                    .debt
                    .clear_isolated_debt(debt_asset, &mut shares, &mut principal, amount_out)?;
            require_eq!(transition.clearance.cash_repaid, amount_out, ErrorCode::BrokenInvariant);
            transition.position_debt_shares = shares;
            transition.position_debt_principal = principal;
            transition.removed_unrealized_interest = transition.clearance.interest_paid;
            cash_debit_out = transition.clearance.interest_paid;
            extra_live_debit_out = transition.clearance.live_debit_for_cash_repay()?;
        }
        SwapCashPolicy::Close {
            debt_asset,
            debt_shares,
            debt_principal,
        } => {
            require!(debt_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
            let mut shares = debt_shares;
            let mut principal = debt_principal;
            transition.clearance =
                market
                    .debt
                    .clear_isolated_debt(debt_asset, &mut shares, &mut principal, u64::MAX)?;
            require_gte!(
                amount_out,
                transition.clearance.cash_repaid,
                ErrorCode::InsufficientAmount
            );
            require_eq!(transition.clearance.remaining_debt, 0, ErrorCode::BrokenInvariant);
            transition.position_debt_shares = shares;
            transition.position_debt_principal = principal;
            transition.removed_unrealized_interest = transition.clearance.interest_paid;
            cash_debit_out = amount_out
                .checked_sub(transition.clearance.cash_repaid)
                .and_then(|residual| residual.checked_add(transition.clearance.interest_paid))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            extra_live_debit_out = transition.clearance.live_debit_for_cash_repay()?;
        }
        SwapCashPolicy::Liquidate {
            debt_asset,
            debt_shares,
            debt_principal,
        } => {
            require!(debt_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
            let full_repayment = market
                .debt
                .isolated_repayment_for_max(debt_asset, debt_shares, u64::MAX)?;
            let position_principal = u64::try_from(debt_principal).map_err(|_| ErrorCode::DebtMathOverflow)?;
            require_gte!(
                full_repayment.cash_repaid,
                position_principal,
                ErrorCode::DebtMathOverflow
            );
            let repay_credit = amount_out.min(full_repayment.cash_repaid);
            let (principal_paid, interest_paid) =
                crate::math::realized_interest_split(repay_credit, full_repayment.cash_repaid as u128, debt_principal)?;
            transition.clearance = DebtClearance {
                shares_burned: debt_shares,
                cash_repaid: repay_credit,
                debt_reduced: full_repayment.position_debt_reduced,
                aggregate_debt_reduced: repay_credit,
                principal_paid,
                interest_paid,
                remaining_debt: 0,
                position_principal_reduced: position_principal,
            };
            transition.writeoff = DebtWriteoff {
                shares_written_off: 0,
                debt_written_off: full_repayment.position_debt_reduced.saturating_sub(repay_credit),
                aggregate_debt_written_off: full_repayment
                    .cash_repaid
                    .checked_sub(repay_credit)
                    .ok_or(ErrorCode::DebtMathOverflow)?,
                principal_written_off: position_principal.saturating_sub(principal_paid),
            };
            debt_curve_reserve_before_share_removal = Some(market.curve_reserve(debt_asset)?);
            let (aggregate_shares, aggregate_principal) = match debt_asset {
                MarketAsset::Base => (
                    &mut market.debt.isolated_base_shares,
                    &mut market.debt.isolated_base_principal,
                ),
                MarketAsset::Quote => (
                    &mut market.debt.isolated_quote_shares,
                    &mut market.debt.isolated_quote_principal,
                ),
            };
            *aggregate_shares = aggregate_shares
                .checked_sub(debt_shares)
                .ok_or(ErrorCode::DebtShareMathOverflow)?;
            *aggregate_principal = aggregate_principal
                .checked_sub(position_principal)
                .ok_or(ErrorCode::DebtMathOverflow)?;
            cash_debit_out = amount_out
                .saturating_sub(full_repayment.cash_repaid)
                .checked_add(interest_paid)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            extra_live_debit_out = transition.clearance.live_debit_for_cash_repay()?;
        }
    }

    {
        let (side_in, side_out) = market.swap_sides_mut(asset_in);
        side_in.reserves.live_reserve = side_in
            .reserves
            .live_reserve
            .checked_add(amount_in_after_fee)
            .ok_or(ErrorCode::ReserveOverflow)?;
        side_in.reserves.cash_reserve = side_in
            .reserves
            .cash_reserve
            .checked_add(amount_in_after_fee)
            .ok_or(ErrorCode::ReserveOverflow)?;
        side_out.reserves.live_reserve = side_out
            .reserves
            .live_reserve
            .checked_sub(
                amount_out
                    .checked_add(extra_live_debit_out)
                    .ok_or(ErrorCode::ReserveUnderflow)?,
            )
            .ok_or(ErrorCode::ReserveUnderflow)?;
        side_out.reserves.cash_reserve = side_out
            .reserves
            .cash_reserve
            .checked_sub(cash_debit_out)
            .ok_or(ErrorCode::CashReserveUnderflow)?;
    }

    if let SwapCashPolicy::Borrow { asset, amount } = policy {
        transition.added_debt_shares = market.add_isolated_borrow_debt(asset, amount)?;
    }
    if let (SwapCashPolicy::Liquidate { debt_asset, .. }, Some(curve_before)) =
        (policy, debt_curve_reserve_before_share_removal)
    {
        let expected_curve_after = curve_before
            .checked_sub(amount_out)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        let curve_after = market.curve_reserve(debt_asset)?;
        require_gte!(curve_after, expected_curve_after, ErrorCode::BrokenInvariant);
        transition.phantom_unpaid_interest = curve_after
            .checked_sub(expected_curve_after)
            .ok_or(ErrorCode::BrokenInvariant)?;
        require_gte!(
            transition.writeoff.aggregate_debt_written_off,
            transition.phantom_unpaid_interest,
            ErrorCode::BrokenInvariant
        );
        transition.removed_unrealized_interest = transition
            .clearance
            .interest_paid
            .checked_add(transition.phantom_unpaid_interest)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        transition.socialized_principal_loss = transition
            .writeoff
            .aggregate_debt_written_off
            .checked_sub(transition.phantom_unpaid_interest)
            .ok_or(ErrorCode::BrokenInvariant)?;
        if transition.phantom_unpaid_interest > 0 {
            market.side_mut(debt_asset).reserves.live_reserve = market
                .side(debt_asset)
                .reserves
                .live_reserve
                .checked_sub(transition.phantom_unpaid_interest)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
    }
    Ok(transition)
}

fn indexed_position(market: &mut Market, debt_asset: MarketAsset, principal: u64) -> LeveragePosition {
    let position = seeded_position(market, debt_asset, principal, principal.saturating_mul(4));
    let debt_before = market.debt.isolated_debt(debt_asset).unwrap();
    match debt_asset {
        MarketAsset::Base => market.debt.base_borrow_index_nad = (NAD as u128) * 3 / 2,
        MarketAsset::Quote => market.debt.quote_borrow_index_nad = (NAD as u128) * 3 / 2,
    }
    let debt_after = market.debt.isolated_debt(debt_asset).unwrap();
    let accrued = u64::try_from(debt_after - debt_before).unwrap();
    market.side_mut(debt_asset).reserves.live_reserve += accrued;
    position
}

fn leverage_lifecycle_cases(asset_in: MarketAsset) -> Vec<(Market, SwapCashPolicy, u64, u64)> {
    let mut cases = vec![(test_market(5_000_000, 5_000_000), SwapCashPolicy::Spot, 123_457, 71_003)];

    let mut borrow_market = test_market(5_000_000, 5_000_000);
    match asset_in {
        MarketAsset::Base => borrow_market.debt.base_borrow_index_nad = (NAD as u128) * 3 / 2,
        MarketAsset::Quote => borrow_market.debt.quote_borrow_index_nad = (NAD as u128) * 3 / 2,
    }
    cases.push((
        borrow_market,
        SwapCashPolicy::Borrow {
            asset: asset_in,
            amount: 100_001,
        },
        150_003,
        80_007,
    ));

    let debt_asset = asset_in.opposite();
    let mut decrease_market = test_market(5_000_000, 5_000_000);
    let decrease_position = indexed_position(&mut decrease_market, debt_asset, 120_001);
    cases.push((
        decrease_market,
        SwapCashPolicy::Decrease {
            debt_asset,
            debt_shares: decrease_position.debt_shares,
            debt_principal: decrease_position.debt_principal,
        },
        53_009,
        40_003,
    ));

    let mut close_market = test_market(5_000_000, 5_000_000);
    let close_position = indexed_position(&mut close_market, debt_asset, 120_001);
    let close_debt = close_position.debt_amount(&close_market.debt).unwrap();
    cases.push((
        close_market,
        SwapCashPolicy::Close {
            debt_asset,
            debt_shares: close_position.debt_shares,
            debt_principal: close_position.debt_principal,
        },
        81_011,
        close_debt + 10_007,
    ));

    let mut liquidation_market = test_market(5_000_000, 5_000_000);
    let liquidation_position = indexed_position(&mut liquidation_market, debt_asset, 120_001);
    cases.push((
        liquidation_market,
        SwapCashPolicy::Liquidate {
            debt_asset,
            debt_shares: liquidation_position.debt_shares,
            debt_principal: liquidation_position.debt_principal,
        },
        61_013,
        70_009,
    ));
    cases
}

fn assert_lifecycle_plan_matches_reference(
    market: Market,
    policy: SwapCashPolicy,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
    amount_out: u64,
) -> LeverageLifecycleTransition {
    let mut reference = market.clone();
    let mut planned = market;
    let reference_transition = apply_leverage_lifecycle_transition_reference(
        &mut reference,
        policy,
        asset_in,
        amount_in_after_fee,
        amount_out,
    )
    .unwrap();
    let before_plan = planned.try_to_vec().unwrap();
    let plan =
        derive_leverage_lifecycle_plan(&planned, policy, asset_in, amount_in_after_fee, amount_out, amount_out)
            .unwrap();
    assert_eq!(planned.try_to_vec().unwrap(), before_plan);
    let planned_transition = apply_leverage_lifecycle_plan(&mut planned, plan).unwrap();
    assert_eq!(planned_transition, reference_transition);
    assert_eq!(planned.try_to_vec().unwrap(), reference.try_to_vec().unwrap());
    if planned_transition.socialized_principal_loss == 0 {
        planned.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
        planned.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    }
    planned_transition
}

#[test]
fn leverage_lifecycle_plan_matches_legacy_for_every_policy_and_asset() {
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        for (market, policy, amount_in_after_fee, amount_out) in leverage_lifecycle_cases(asset_in) {
            let transition =
                assert_lifecycle_plan_matches_reference(market, policy, asset_in, amount_in_after_fee, amount_out);
            match policy {
                SwapCashPolicy::Spot => assert_eq!(transition, LeverageLifecycleTransition::default()),
                SwapCashPolicy::Borrow { .. } => assert!(transition.added_debt_shares > 0),
                SwapCashPolicy::Decrease { .. } => {
                    assert!(transition.clearance.interest_paid > 0);
                    assert!(transition.position_debt_shares > 0);
                }
                SwapCashPolicy::Close { .. } => {
                    assert!(transition.clearance.interest_paid > 0);
                    assert_eq!(transition.position_debt_shares, 0);
                }
                SwapCashPolicy::Liquidate { .. } => {
                    assert!(transition.phantom_unpaid_interest > 0);
                    assert!(transition.socialized_principal_loss > 0);
                }
            }
        }
    }
}

#[test]
fn leverage_lifecycle_plan_rejects_stale_and_tampered_inputs_atomically() {
    fn assert_rejected_atomically(mut market: Market, plan: LeverageLifecyclePlan) {
        let before = market.try_to_vec().unwrap();
        let error = apply_leverage_lifecycle_plan(&mut market, plan).unwrap_err();
        assert_eq!(error, error!(ErrorCode::BrokenInvariant));
        assert_eq!(market.try_to_vec().unwrap(), before);
    }

    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        for (market, policy, amount_in_after_fee, amount_out) in leverage_lifecycle_cases(asset) {
            let plan =
                derive_leverage_lifecycle_plan(&market, policy, asset, amount_in_after_fee, amount_out, amount_out)
                    .unwrap();

            let mut stale = market.clone();
            stale.base_side.reserves.cash_reserve += 1;
            assert_rejected_atomically(stale, plan);

            let mut forged_post = plan;
            forged_post.post.base_live_reserve += 1;
            assert_rejected_atomically(market.clone(), forged_post);

            let mut forged_transition = plan;
            forged_transition.transition.removed_unrealized_interest += 1;
            assert_rejected_atomically(market, forged_transition);
        }

        let (borrow_market, borrow_policy, amount_in_after_fee, amount_out) = leverage_lifecycle_cases(asset).remove(1);
        let plan =
            derive_leverage_lifecycle_plan(
                &borrow_market,
                borrow_policy,
                asset,
                amount_in_after_fee,
                amount_out,
                amount_out,
            )
                .unwrap();
        for field in 0..10 {
            let mut stale = borrow_market.clone();
            match field {
                0 => stale.base_side.reserves.live_reserve += 1,
                1 => stale.base_side.reserves.cash_reserve += 1,
                2 => stale.quote_side.reserves.live_reserve += 1,
                3 => stale.quote_side.reserves.cash_reserve += 1,
                4 => stale.debt.base_borrow_index_nad += 1,
                5 => stale.debt.quote_borrow_index_nad += 1,
                6 => stale.debt.isolated_base_shares += 1,
                7 => stale.debt.isolated_quote_shares += 1,
                8 => stale.debt.isolated_base_principal += 1,
                9 => stale.debt.isolated_quote_principal += 1,
                _ => unreachable!(),
            }
            assert_rejected_atomically(stale, plan);
        }

        let (liquidation_market, liquidation_policy, amount_in_after_fee, amount_out) =
            leverage_lifecycle_cases(asset).remove(4);
        let liquidation_plan = derive_leverage_lifecycle_plan(
            &liquidation_market,
            liquidation_policy,
            asset,
            amount_in_after_fee,
            amount_out,
            amount_out,
        )
        .unwrap();
        let mut hidden_stale = liquidation_market;
        match asset.opposite() {
            MarketAsset::Base => hidden_stale.debt.fixed_base_shares += 1,
            MarketAsset::Quote => hidden_stale.debt.fixed_quote_shares += 1,
        }
        assert_rejected_atomically(hidden_stale, liquidation_plan);

        let mut impossible = test_market(100, 100);
        let impossible_before = impossible.try_to_vec().unwrap();
        assert!(impossible
            .apply_leverage_lifecycle_transition(SwapCashPolicy::Borrow { asset, amount: 101 }, asset, 1, 1, 1)
            .is_err());
        assert_eq!(impossible.try_to_vec().unwrap(), impossible_before);
    }
}

fn prepare_leverage_swap_with_policy(
    market: &mut Market,
    asset_in: MarketAsset,
    reserve_credit: u64,
    current_slot: u64,
    cash_policy: SwapCashPolicy,
) -> PreparedLeverageSwap {
    let prepared = SwapRequest {
        current_slot,
        current_unix_timestamp: 0,
        asset_in,
        reserve_credit,
        protocol_fee_bps: 0,
    }
    .prepare_with_cash_policy(market, cash_policy)
    .unwrap();
    PreparedLeverageSwap {
        concentrated_transition: prepared.concentrated_transition,
        swap: LeverageSwapQuote::from_amm(prepared.quote, current_slot),
        base_pre_rebalance: prepared.base_pre_rebalance,
        quote_pre_rebalance: prepared.quote_pre_rebalance,
        fee_eligible_ylp_supply: prepared.fee_eligible_ylp_supply,
        interest_eligibility: prepared.interest_eligibility,
        cash_policy: prepared.cash_policy,
    }
}

fn assert_hlp_combined_tracking_budget(
    market: &Market,
    base_final: HlpRebalanceReceipt,
    quote_final: HlpRebalanceReceipt,
) {
    let prices = crate::market::liquidity::current_hlp_curve_prices(market).unwrap();
    for receipt in [base_final, quote_final] {
        if receipt.tracking_loss_budget_nad == 0 {
            assert_eq!(receipt.residual_exposure, 0);
            continue;
        }
        assert!(receipt.tracking_loss_budget_nad > 0);
        let combined_delta = crate::market::liquidity::hlp_end_to_end_tracking_delta(market, receipt, prices).unwrap();
        assert!(combined_delta.unsigned_abs() <= receipt.tracking_loss_budget_nad);
    }
}

fn concentrated_market() -> Market {
    let mut market = test_market(1_000_000, 1_000_000);
    market.config.swap_fee_bps = 30;
    market.config.divergence_fee_share_cap_bps = 2_000;
    market.config.volatility_fee_share_cap_bps = 2_000;
    market.config.amm = AmmConfig {
        peak_amplification_nad: 4 * NAD,
        core_half_width_bps: 100,
        fade_width_bps: 400,
        center_ema_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_shock_cap_nad: NAD / 10,
        volatility_cap_nad: NAD,
        divergence_fee_coefficient_nad: 10 * NAD,
        volatility_fee_coefficient_nad: NAD / 10,
        ..AmmConfig::default()
    };
    market.amm = AmmState::default();
    market.prepare_amm_for_swap(1).unwrap();
    market
}

fn active_concentrated_hlp_market_with_decimals(decimals: u8) -> Market {
    let scale = 10_u64.pow(decimals as u32);
    let mut market = test_market(1_000_000 * scale, 1_000_000 * scale);
    market.base_side.asset_decimals = decimals;
    market.quote_side.asset_decimals = decimals;
    market.config.target_hlp_leverage_bps = 20_000;
    // Lifecycle success fixtures intentionally use the widest valid band. A
    // separate test below proves that worsening flow outside a narrow band is
    // rejected.
    market.config.settlement_divergence_bps = 10_000;
    market
        .deposit_single_sided(MarketAsset::Base, 100_000 * scale, 1)
        .unwrap();
    market
        .deposit_single_sided(MarketAsset::Quote, 100_000 * scale, 1)
        .unwrap();
    market.config.swap_fee_bps = 30;
    market.config.divergence_fee_share_cap_bps = 2_000;
    market.config.volatility_fee_share_cap_bps = 2_000;
    market.config.amm = AmmConfig {
        peak_amplification_nad: 4 * NAD,
        core_half_width_bps: 100,
        fade_width_bps: 400,
        center_ema_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_shock_cap_nad: NAD / 10,
        volatility_cap_nad: NAD,
        divergence_fee_coefficient_nad: 10 * NAD,
        volatility_fee_coefficient_nad: NAD / 10,
        ..AmmConfig::default()
    };
    market.amm = AmmState::default();
    market.prepare_amm_for_swap(1).unwrap();
    assert!(market.base_hlp_vault.hlp_supply > 0);
    assert!(market.quote_hlp_vault.hlp_supply > 0);
    assert!(market.config.amm.peak_amplification_nad > NAD);
    market
}

fn active_concentrated_hlp_market() -> Market {
    active_concentrated_hlp_market_with_decimals(0)
}

fn active_reconfigured_concentrated_hlp_market_with_decimals(decimals: u8) -> Market {
    let mut market = active_concentrated_hlp_market_with_decimals(decimals);
    market.config.amm.peak_amplification_nad = NAD;
    market.config.amm.core_half_width_bps = 0;
    market.config.amm.fade_width_bps = 0;
    market
        .config
        .amm
        .set_concentrated_curve_parameters(ConcentratedCurveParameters {
            peak_amplification_nad: 4 * NAD,
            core_half_width_bps: 100,
            fade_width_bps: 400,
        })
        .unwrap();
    market.amm = AmmState::default();
    market.prepare_amm_for_swap(1).unwrap();
    market
}

#[test]
fn concentrated_hlp_deposit_rebases_curve_without_legacy_solver() {
    let scale = 1_000_000;
    let mut market = test_market(1_000_000 * scale, 1_000_000 * scale);
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    market.config.settlement_divergence_bps = 10_000;
    market.config.amm.peak_amplification_nad = NAD;
    market.config.amm.core_half_width_bps = 0;
    market.config.amm.fade_width_bps = 0;
    market
        .config
        .amm
        .set_concentrated_curve_parameters(ConcentratedCurveParameters {
            peak_amplification_nad: 4 * NAD,
            core_half_width_bps: 100,
            fade_width_bps: 400,
        })
        .unwrap();
    market.amm = AmmState::default();
    market.prepare_amm_for_swap(1).unwrap();
    let before = market.amm.concentrated_curve_cache;
    market
        .deposit_single_sided(MarketAsset::Base, 100_000 * scale, 1)
        .unwrap();
    market.finalize_amm_transition_and_observe_risk(2).unwrap();
    assert_eq!(market.amm.concentrated_curve_cache, before);
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    market.assert_market_invariants().unwrap();
}

#[test]
fn concentrated_hlp_withdrawal_preserves_integrated_hedge_and_reserve_identity() {
    let mut market = active_reconfigured_concentrated_hlp_market_with_decimals(6);
    let amount = market.base_hlp_vault.hlp_supply / 10;
    let receipt = market.withdraw_single_sided(MarketAsset::Base, amount).unwrap();
    assert!(receipt.target_amount_out > 0);
    market.finalize_amm_transition_and_observe_risk(2).unwrap();
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    market.assert_market_invariants().unwrap();
}

#[test]
fn concentrated_hlp_transition_consumes_accrued_funding_interest_once() {
    let mut market = active_reconfigured_concentrated_hlp_market_with_decimals(6);
    market.debt.base_borrow_index_nad = (NAD as u128) * 11 / 10;
    market.debt.quote_borrow_index_nad = (NAD as u128) * 11 / 10;

    let base_interest = u64::try_from(
        Debt::shares_to_debt(
            market.base_hlp_vault.debt_shares,
            market.debt.quote_borrow_index_nad,
        )
        .unwrap(),
    )
    .unwrap()
    .saturating_sub(market.base_hlp_vault.debt_principal);
    let quote_interest = u64::try_from(
        Debt::shares_to_debt(
            market.quote_hlp_vault.debt_shares,
            market.debt.base_borrow_index_nad,
        )
        .unwrap(),
    )
    .unwrap()
    .saturating_sub(market.quote_hlp_vault.debt_principal);

    assert!(base_interest > 3);
    assert!(quote_interest > 3);
    market.assert_market_invariants().unwrap();

    let transition = prepare_concentrated_hlp_transition_at_current_state(&market).unwrap();
    let (base_receipt, quote_receipt) = transition.consume(&mut market).unwrap();

    assert_eq!(base_receipt.interest_paid, base_interest);
    assert_eq!(quote_receipt.interest_paid, quote_interest);
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    assert_eq!(market.quote_hlp_vault.residual_exposure, 0);
    market.assert_market_invariants().unwrap();
}

fn assert_exact_concentrated_hlp_residual_exposure(market: &Market) {
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    assert_eq!(market.quote_hlp_vault.residual_exposure, 0);
}

fn assert_final_leverage_risk_observation(market: &Market, current_slot: u64, revision_before: u64) {
    let final_price_nad = market.current_concentrated_spot_price_nad().unwrap().unwrap();

    assert_eq!(market.curve_revision, revision_before + 1);
    assert_eq!(market.risk_revision, revision_before);
    assert_eq!(market.risk.last_snapshot_slot, current_slot);
    assert_eq!(market.risk.cached_spot_base_price_nad, final_price_nad);
    assert_eq!(market.last_marginal_observation_nad, final_price_nad);
    assert!(market.amm.concentrated_curve_cache.tail_liquidity > 0);
}

#[test]
fn open_leverage_tracks_isolated_debt_and_cash() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = empty_position();
    let quote = market.quote_leverage_swap(MarketAsset::Base, 2_000, 1).unwrap();
    let fee_credit = full_fee_credit(&quote);

    let receipt = market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::default(),
            0,
            MarketAsset::Base,
            1_000,
            20_000,
            quote.amount_out,
            prepared_leverage_swap(
                &market,
                quote,
                SwapCashPolicy::Borrow {
                    asset: MarketAsset::Base,
                    amount: 1_000,
                },
            ),
            fee_credit,
            0,
            1,
            255,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

    assert_eq!(receipt.debt_amount, 1_000);
    assert_eq!(position.debt_shares, 1_000);
    assert_eq!(market.debt.isolated_base_shares, 1_000);
    assert_eq!(market.debt.fixed_base_shares, 0);
    assert_eq!(
        market.base_side.reserves.live_reserve,
        1_000_000 + quote.reserve_input_credit
    );
    assert_eq!(
        market.base_side.reserves.cash_reserve,
        1_000_000 - 1_000 + quote.reserve_input_credit
    );
    assert_eq!(market.quote_side.reserves.live_reserve, 1_000_000 - quote.amount_out);
    assert_eq!(market.quote_side.reserves.cash_reserve, 1_000_000 - quote.amount_out);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn referred_leverage_records_exact_debt_and_binds_partner() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = empty_position();
    let referral_partner = Pubkey::new_unique();
    let open_quote = market.quote_leverage_swap(MarketAsset::Base, 2_000, 1).unwrap();
    let open_fee_credit = full_fee_credit(&open_quote);

    let open = market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            referral_partner,
            2_500,
            MarketAsset::Base,
            1_000,
            20_000,
            open_quote.amount_out,
            prepared_leverage_swap(
                &market,
                open_quote,
                SwapCashPolicy::Borrow {
                    asset: MarketAsset::Base,
                    amount: 1_000,
                },
            ),
            open_fee_credit,
            0,
            1,
            255,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

    assert_eq!(open.borrowed_amount, 1_000);
    assert_eq!(open.debt_amount, 1_000);
    assert_eq!(open.swap.amount_in, 2_000);
    assert_eq!(position.referral_partner, referral_partner);
    assert_eq!(position.referral_interest_share_bps, 2_500);
    assert_eq!(position.debt_principal, 1_000);
    assert_eq!(market.debt.isolated_base_principal, 1_000);
    assert_eq!(market.base_side.daily_borrow_bucket.borrowed_bucket, 0);

    let increase_quote = market.quote_leverage_swap(MarketAsset::Base, 100, 2).unwrap();
    let increase_fee_credit = full_fee_credit(&increase_quote);
    let increase = market
        .increase_leverage(
            &mut position,
            100,
            increase_quote.amount_out,
            prepared_leverage_swap(
                &market,
                increase_quote,
                SwapCashPolicy::Borrow {
                    asset: MarketAsset::Base,
                    amount: 100,
                },
            ),
            increase_fee_credit,
            0,
            ProtocolAuctionSplit::default(),
            2,
            0,
        )
        .unwrap();
    assert_eq!(increase.borrowed_amount, 100);
    assert_eq!(increase.debt_delta, 100);
    assert_eq!(position.referral_partner, referral_partner);
    assert_eq!(position.referral_interest_share_bps, 2_500);
    assert_eq!(market.base_side.daily_borrow_bucket.borrowed_bucket, 0);
}

#[test]
fn remove_leverage_margin_does_not_consume_the_public_borrow_bucket() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 100, 1_000);

    let receipt = market.remove_leverage_margin(&mut position, 10, 0, 0).unwrap();

    assert_eq!(receipt.borrowed_amount, 10);
    assert_eq!(receipt.debt_delta, 10);
    assert_eq!(market.base_side.daily_borrow_bucket.borrowed_bucket, 0);
}

#[test]
fn isolated_leverage_ignores_capacity_used_by_public_borrowers() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 100, 1_000);
    let remaining_for_isolated = 9;
    let limit = market
        .daily_limit_for_side(MarketAsset::Base, market.config.max_daily_borrow_bps)
        .unwrap();
    market
        .side_mut(MarketAsset::Base)
        .daily_borrow_bucket
        .record_borrow(limit - remaining_for_isolated, limit, 0)
        .unwrap();

    let receipt = market.remove_leverage_margin(&mut position, 10, 0, 0).unwrap();

    assert_eq!(receipt.borrowed_amount, 10);
    assert_eq!(market.base_side.daily_borrow_bucket.borrowed_bucket, limit - 9);
}

#[test]
fn close_leverage_clears_isolated_debt_and_residual_cash() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = empty_position();
    let open_quote = market.quote_leverage_swap(MarketAsset::Base, 2_000, 1).unwrap();
    let open_fee_credit = full_fee_credit(&open_quote);
    market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::default(),
            0,
            MarketAsset::Base,
            1_000,
            20_000,
            open_quote.amount_out,
            prepared_leverage_swap(
                &market,
                open_quote,
                SwapCashPolicy::Borrow {
                    asset: MarketAsset::Base,
                    amount: 1_000,
                },
            ),
            open_fee_credit,
            0,
            1,
            255,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();
    let base_cash_before_close = market.base_side.reserves.cash_reserve;
    let close_quote = market
        .quote_leverage_swap(MarketAsset::Quote, position.collateral_amount, 2)
        .unwrap();
    let close_fee_credit = full_fee_credit(&close_quote);
    let prepared_close = prepared_leverage_swap(
        &market,
        close_quote,
        SwapCashPolicy::Close {
            debt_asset: MarketAsset::Base,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
        },
    );

    let receipt = market
        .close_leverage(
            &mut position,
            0,
            prepared_close,
            close_fee_credit,
            0,
            ProtocolAuctionSplit::default(),
            2,
        )
        .unwrap();

    assert_eq!(receipt.debt_repaid, 1_000);
    assert_eq!(market.debt.isolated_base_shares, 0);
    assert_eq!(market.debt.isolated_base_principal, 0);
    assert_eq!(position.debt_shares, 0);
    assert_eq!(position.collateral_amount, 0);
    assert_eq!(
        market.base_side.reserves.cash_reserve,
        base_cash_before_close - receipt.residual
    );
    assert_eq!(receipt.closeout_value, close_quote.amount_out);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn leverage_close_slice_rounds_debt_up_and_preserves_full_close_exactness() {
    let mut market = test_market(1_000_000, 1_000_000);
    let position = seeded_position(&mut market, MarketAsset::Base, 3, 7);

    let partial = market.leverage_close_slice(&position, 3_333).unwrap();
    assert_eq!(partial.collateral_amount, 2);
    assert_eq!(partial.debt_shares, 1);
    assert_eq!(partial.debt_principal, 1);

    let full = market.leverage_close_slice(&position, BPS_DENOMINATOR).unwrap();
    assert_eq!(full.collateral_amount, position.collateral_amount);
    assert_eq!(full.debt_shares, position.debt_shares);
    assert_eq!(full.debt_principal, position.debt_principal);
    assert!(market.leverage_close_slice(&position, 0).is_err());
}

#[test]
fn partial_close_leverage_pays_equity_and_keeps_the_remainder_open() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = empty_position();
    let open_quote = market.quote_leverage_swap(MarketAsset::Base, 2_000, 1).unwrap();
    market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::default(),
            0,
            MarketAsset::Base,
            1_000,
            20_000,
            open_quote.amount_out,
            prepared_leverage_swap(
                &market,
                open_quote,
                SwapCashPolicy::Borrow {
                    asset: MarketAsset::Base,
                    amount: 1_000,
                },
            ),
            full_fee_credit(&open_quote),
            0,
            1,
            255,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

    let position_before = position.clone();
    let aggregate_shares_before = market.debt.isolated_base_shares;
    let aggregate_principal_before = market.debt.isolated_base_principal;
    let slice = market.leverage_close_slice(&position, 5_000).unwrap();
    let close_quote = market
        .quote_leverage_swap(MarketAsset::Quote, slice.collateral_amount, 2)
        .unwrap();
    let prepared_close = prepared_leverage_swap(
        &market,
        close_quote,
        SwapCashPolicy::Close {
            debt_asset: MarketAsset::Base,
            debt_shares: slice.debt_shares,
            debt_principal: slice.debt_principal,
        },
    );

    let receipt = market
        .partial_close_leverage(
            &mut position,
            5_000,
            0,
            prepared_close,
            full_fee_credit(&close_quote),
            0,
            ProtocolAuctionSplit::default(),
            2,
            0,
        )
        .unwrap();

    assert_eq!(receipt.collateral_sold, slice.collateral_amount);
    assert_eq!(receipt.closeout_value, close_quote.amount_out);
    assert_eq!(receipt.residual, close_quote.amount_out - receipt.debt_repaid);
    assert_eq!(
        position.collateral_amount,
        position_before.collateral_amount - slice.collateral_amount
    );
    assert_eq!(position.debt_shares, position_before.debt_shares - slice.debt_shares);
    assert_eq!(
        position.debt_principal,
        position_before.debt_principal - slice.debt_principal
    );
    assert_eq!(receipt.remaining_collateral_amount, position.collateral_amount);
    assert_eq!(receipt.remaining_debt_shares, position.debt_shares);
    assert_eq!(
        receipt.debt_reduced,
        position_before.debt_amount(&market.debt).unwrap() - receipt.remaining_debt_amount
    );
    assert_eq!(
        market.debt.isolated_base_shares,
        aggregate_shares_before - slice.debt_shares
    );
    assert_eq!(
        market.debt.isolated_base_principal,
        aggregate_principal_before - u64::try_from(slice.debt_principal).unwrap()
    );
    assert!(receipt.remaining_debt_amount > 0);
    assert!(receipt.remaining_closeout_value > receipt.remaining_debt_amount);
    position.require_open().unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn add_margin_never_reduces_more_debt_than_the_cash_repaid() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 100, 10_000);
    market.debt.base_borrow_index_nad = (NAD as u128) * 3 / 2;
    market.base_side.reserves.live_reserve += 50;
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    let live_before = market.base_side.reserves.live_reserve;
    let cash_before = market.base_side.reserves.cash_reserve;

    let receipt = market.add_leverage_margin(&mut position, 2, 1).unwrap();

    assert_eq!(receipt.debt_delta, -2);
    assert_eq!(receipt.debt_amount, 148);
    assert_eq!(receipt.interest_paid, 1);
    assert_eq!(position.debt_shares, 99);
    assert_eq!(position.debt_principal, 99);
    assert_eq!(market.debt.isolated_base_shares, 99);
    assert_eq!(market.debt.isolated_base_principal, 99);
    assert_eq!(market.base_side.reserves.live_reserve, live_before - 1);
    assert_eq!(market.base_side.reserves.cash_reserve, cash_before + 1);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn solvent_liquidation_closes_position_and_pays_residual_incentive() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 1_010);
    let quote = market
        .quote_leverage_swap(MarketAsset::Quote, position.collateral_amount, 1)
        .unwrap();
    let fee_credit = full_fee_credit(&quote);
    let prepared_liquidation = prepared_leverage_swap(
        &market,
        quote,
        SwapCashPolicy::Liquidate {
            debt_asset: MarketAsset::Base,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
        },
    );

    let receipt = market
        .liquidate_leverage_position(
            &mut position,
            prepared_liquidation,
            fee_credit,
            0,
            ProtocolAuctionSplit::default(),
            1,
        )
        .unwrap();

    assert_eq!(market.debt.isolated_base_shares, 0);
    assert_eq!(position.debt_shares, 0);
    assert_eq!(position.collateral_amount, 0);
    assert_eq!(receipt.debt_repaid, 1_000);
    assert_eq!(receipt.principal_written_off, 0);
    assert!(receipt.liquidator_amount > 0);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn insolvent_liquidation_socializes_unrepaid_principal() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 500);
    let quote = market
        .quote_leverage_swap(MarketAsset::Quote, position.collateral_amount, 1)
        .unwrap();
    let fee_credit = full_fee_credit(&quote);
    let prepared_liquidation = prepared_leverage_swap(
        &market,
        quote,
        SwapCashPolicy::Liquidate {
            debt_asset: MarketAsset::Base,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
        },
    );

    let receipt = market
        .liquidate_leverage_position(
            &mut position,
            prepared_liquidation,
            fee_credit,
            0,
            ProtocolAuctionSplit::default(),
            1,
        )
        .unwrap();

    assert_eq!(market.debt.isolated_base_shares, 0);
    assert_eq!(position.debt_shares, 0);
    assert_eq!(position.collateral_amount, 0);
    assert!(receipt.debt_repaid < 1_000);
    assert!(receipt.principal_written_off > 0);
    assert_eq!(receipt.liquidator_amount, 0);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn insolvent_liquidation_does_not_socialize_phantom_unpaid_interest() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 500);
    let surviving_position = seeded_position(&mut market, MarketAsset::Base, 333, 1_000);
    let aggregate_debt_before_accrual = market.debt.isolated_debt(MarketAsset::Base).unwrap();
    market.debt.base_borrow_index_nad = 6 * NAD as u128 / 5;
    let aggregate_debt_before = market.debt.isolated_debt(MarketAsset::Base).unwrap();
    let accrued = u64::try_from(aggregate_debt_before - aggregate_debt_before_accrual).unwrap();
    market.base_side.reserves.live_reserve += accrued;
    let curve_before = market.curve_reserve(MarketAsset::Base).unwrap();
    let aggregate_shares_before = market.debt.isolated_base_shares;
    let aggregate_principal_before = market.debt.isolated_base_principal;
    let full_repayment = market
        .debt
        .isolated_repayment_for_max(MarketAsset::Base, position.debt_shares, u64::MAX)
        .unwrap();
    let quote = market
        .quote_leverage_swap(MarketAsset::Quote, position.collateral_amount, 1)
        .unwrap();
    let repay_credit = quote.amount_out.min(full_repayment.cash_repaid);
    let (_, interest_paid) = crate::math::realized_interest_split(
        repay_credit,
        (full_repayment.cash_repaid as u128).max(position.debt_principal),
        position.debt_principal,
    )
    .unwrap();
    let aggregate_shares_after = aggregate_shares_before - position.debt_shares;
    let aggregate_debt_after = Debt::shares_to_debt(aggregate_shares_after, market.debt.base_borrow_index_nad).unwrap();
    let aggregate_principal_after = aggregate_principal_before - u64::try_from(position.debt_principal).unwrap();
    let unrealized_before = aggregate_debt_before - u128::from(aggregate_principal_before);
    let unrealized_after = aggregate_debt_after - u128::from(aggregate_principal_after);
    let phantom_unpaid_interest = u64::try_from(unrealized_before - unrealized_after).unwrap() - interest_paid;
    let aggregate_writeoff = full_repayment.cash_repaid - repay_credit;
    let socialized_principal = aggregate_writeoff - phantom_unpaid_interest;
    let prepared_liquidation = prepared_leverage_swap(
        &market,
        quote,
        SwapCashPolicy::Liquidate {
            debt_asset: MarketAsset::Base,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
        },
    );

    let receipt = market
        .liquidate_leverage_position(
            &mut position,
            prepared_liquidation,
            full_fee_credit(&quote),
            0,
            ProtocolAuctionSplit::default(),
            1,
        )
        .unwrap();

    assert!(phantom_unpaid_interest > 0);
    assert!(socialized_principal > 0);
    assert_eq!(receipt.interest_paid, interest_paid);
    assert_eq!(market.debt.isolated_base_shares, surviving_position.debt_shares);
    assert_eq!(
        market.curve_reserve(MarketAsset::Base).unwrap(),
        curve_before - quote.amount_out - socialized_principal
    );
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn pure_unpaid_interest_writeoff_refreshes_the_stored_curve_checkpoint() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 500);
    position.debt_principal = 0;
    market.debt.isolated_base_principal = 0;
    let quote = market
        .quote_leverage_swap(MarketAsset::Quote, position.collateral_amount, 1)
        .unwrap();
    let prepared_liquidation = prepared_leverage_swap(
        &market,
        quote,
        SwapCashPolicy::Liquidate {
            debt_asset: MarketAsset::Base,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
        },
    );

    let receipt = market
        .liquidate_leverage_position(
            &mut position,
            prepared_liquidation,
            full_fee_credit(&quote),
            0,
            ProtocolAuctionSplit::default(),
            1,
        )
        .unwrap();
    assert_eq!(receipt.principal_written_off, 0);
    assert!(receipt.interest_paid > 0 && receipt.debt_repaid < 1_000);
    assert!(market.amm.concentrated_curve_cache.tail_liquidity > 0);
    assert!(market.current_concentrated_spot_price_nad().unwrap().is_some());
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn leverage_quote_is_the_same_concentrated_and_dynamic_fee_quote_as_spot() {
    let mut market = concentrated_market();
    let leverage = market.quote_leverage_swap(MarketAsset::Base, 50_000, 1).unwrap();
    let spot = SwapRequest {
        current_slot: 1,
        current_unix_timestamp: 0,
        asset_in: MarketAsset::Base,
        reserve_credit: 50_000,
        protocol_fee_bps: 0,
    }
    .prepare(&mut market)
    .unwrap()
    .quote;

    assert_eq!(leverage.amount_out, spot.amount_out);
    assert_eq!(leverage.amount_in_after_fee, spot.fee.amount_in_for_quote);
    assert_eq!(leverage.reserve_input_credit, spot.fee.reserve_input_credit);
    assert_eq!(leverage.end_price_nad, spot.end_price_nad);
    assert_eq!(leverage.reserve_end_price_nad, spot.reserve_end_price_nad);
    assert_eq!(leverage.fee_breakdown, spot.fee);
    assert!(leverage.fee_breakdown.divergence_surcharge_debit > 0);
}

#[test]
fn concentrated_open_leverage_checkpoints_active_hlp_exposure() {
    let scale = 1_000_000;
    let mut market = active_concentrated_hlp_market_with_decimals(6);
    let mut position = empty_position();
    let prepared = prepare_leverage_swap_with_policy(
        &mut market,
        MarketAsset::Base,
        2_000 * scale,
        1,
        SwapCashPolicy::Borrow {
            asset: MarketAsset::Base,
            amount: 1_000 * scale,
        },
    );
    let quote = prepared.swap;
    let revision_before = market.curve_revision;

    let receipt = market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::default(),
            0,
            MarketAsset::Base,
            1_000 * scale,
            20_000,
            quote.amount_out,
            prepared,
            full_fee_credit(&quote),
            0,
            1,
            255,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

    assert_hlp_combined_tracking_budget(&market, receipt.base_hlp_rebalance, receipt.quote_hlp_rebalance);
    assert_exact_concentrated_hlp_residual_exposure(&market);
    assert_final_leverage_risk_observation(&market, 1, revision_before);
}

#[test]
fn concentrated_open_leverage_uses_integrated_hlp_transition() {
    let scale = 1_000_000;
    let mut market = active_reconfigured_concentrated_hlp_market_with_decimals(6);
    let mut position = empty_position();
    let prepared = prepare_leverage_swap_with_policy(
        &mut market,
        MarketAsset::Base,
        2_000 * scale,
        1,
        SwapCashPolicy::Borrow {
            asset: MarketAsset::Base,
            amount: 1_000 * scale,
        },
    );
    let quote = prepared.swap;
    let receipt = market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::default(),
            0,
            MarketAsset::Base,
            1_000 * scale,
            20_000,
            quote.amount_out,
            prepared,
            full_fee_credit(&quote),
            0,
            1,
            255,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();
    assert!(receipt.base_hlp_rebalance.residual_exposure == 0);
    assert!(receipt.quote_hlp_rebalance.residual_exposure == 0);
    market.assert_market_invariants().unwrap();
    assert_eq!(
        market.risk.cached_spot_base_price_nad,
        market.current_concentrated_spot_price_nad().unwrap().unwrap()
    );
}

#[test]
fn concentrated_leverage_liquidation_uses_the_same_integrated_transition() {
    let scale = 1_000_000;
    let mut market = active_reconfigured_concentrated_hlp_market_with_decimals(6);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000 * scale, 1_010 * scale);
    let prepared = prepare_leverage_swap_with_policy(
        &mut market,
        MarketAsset::Quote,
        position.collateral_amount,
        2,
        SwapCashPolicy::Liquidate {
            debt_asset: MarketAsset::Base,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
        },
    );
    let fee_credit = full_fee_credit(&prepared.swap);
    let receipt = market
        .liquidate_leverage_position(
            &mut position,
            prepared,
            fee_credit,
            0,
            ProtocolAuctionSplit::default(),
            2,
        )
        .unwrap();
    assert_eq!(position.debt_shares, 0);
    assert!(receipt.debt_repaid > 0);
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    assert_eq!(market.quote_hlp_vault.residual_exposure, 0);
    market.assert_market_invariants().unwrap();
}

#[test]
fn concentrated_socialized_loss_rebases_curve_then_restores_exact_hlp_hedges() {
    let scale = 1_000_000;
    let mut market = active_reconfigured_concentrated_hlp_market_with_decimals(6);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000 * scale, 100 * scale);
    let revision_before = market.curve_revision;
    let prepared = prepare_leverage_swap_with_policy(
        &mut market,
        MarketAsset::Quote,
        position.collateral_amount,
        2,
        SwapCashPolicy::Liquidate {
            debt_asset: MarketAsset::Base,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
        },
    );
    let fee_credit = full_fee_credit(&prepared.swap);
    let receipt = market
        .liquidate_leverage_position(
            &mut position,
            prepared,
            fee_credit,
            0,
            ProtocolAuctionSplit::default(),
            2,
        )
        .unwrap();

    assert!(receipt.principal_written_off > 0);
    assert!(market.curve_revision > revision_before);
    assert_eq!(receipt.base_hlp_rebalance.residual_exposure, 0);
    assert_eq!(receipt.quote_hlp_rebalance.residual_exposure, 0);
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    assert_eq!(market.quote_hlp_vault.residual_exposure, 0);
    assert!(market.current_concentrated_spot_price_nad().unwrap().is_some());
    market.assert_market_invariants().unwrap();
}

#[test]
fn concentrated_increase_leverage_checkpoints_active_hlp_exposure() {
    let scale = 1_000_000;
    let mut market = active_concentrated_hlp_market_with_decimals(6);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000 * scale, 10_000 * scale);
    let prepared = prepare_leverage_swap_with_policy(
        &mut market,
        MarketAsset::Base,
        100 * scale,
        1,
        SwapCashPolicy::Borrow {
            asset: MarketAsset::Base,
            amount: 100 * scale,
        },
    );
    let quote = prepared.swap;
    let revision_before = market.curve_revision;

    let receipt = market
        .increase_leverage(
            &mut position,
            100 * scale,
            quote.amount_out,
            prepared,
            full_fee_credit(&quote),
            0,
            ProtocolAuctionSplit::default(),
            1,
            0,
        )
        .unwrap();

    assert_hlp_combined_tracking_budget(&market, receipt.base_hlp_rebalance, receipt.quote_hlp_rebalance);
    assert_exact_concentrated_hlp_residual_exposure(&market);
    assert_final_leverage_risk_observation(&market, 1, revision_before);
}

#[test]
fn concentrated_decrease_leverage_checkpoints_active_hlp_exposure() {
    let scale = 1_000_000;
    let mut market = active_concentrated_hlp_market_with_decimals(6);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000 * scale, 10_000 * scale);
    let prepared = prepare_leverage_swap_with_policy(
        &mut market,
        MarketAsset::Quote,
        100 * scale,
        1,
        SwapCashPolicy::Decrease {
            debt_asset: MarketAsset::Base,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
        },
    );
    let quote = prepared.swap;
    let revision_before = market.curve_revision;

    let receipt = market
        .decrease_leverage(
            &mut position,
            100 * scale,
            0,
            prepared,
            full_fee_credit(&quote),
            0,
            ProtocolAuctionSplit::default(),
            1,
            0,
        )
        .unwrap();

    assert_hlp_combined_tracking_budget(&market, receipt.base_hlp_rebalance, receipt.quote_hlp_rebalance);
    assert_exact_concentrated_hlp_residual_exposure(&market);
    assert_final_leverage_risk_observation(&market, 1, revision_before);
}

#[test]
fn concentrated_close_leverage_checkpoints_active_hlp_exposure() {
    let scale = 1_000_000;
    let mut market = active_concentrated_hlp_market_with_decimals(6);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000 * scale, 2_000 * scale);
    let prepared = prepare_leverage_swap_with_policy(
        &mut market,
        MarketAsset::Quote,
        position.collateral_amount,
        1,
        SwapCashPolicy::Close {
            debt_asset: MarketAsset::Base,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
        },
    );
    let quote = prepared.swap;
    let revision_before = market.curve_revision;

    let receipt = market
        .close_leverage(
            &mut position,
            0,
            prepared,
            full_fee_credit(&quote),
            0,
            ProtocolAuctionSplit::default(),
            1,
        )
        .unwrap();

    assert_hlp_combined_tracking_budget(&market, receipt.base_hlp_rebalance, receipt.quote_hlp_rebalance);
    assert_exact_concentrated_hlp_residual_exposure(&market);
    assert_final_leverage_risk_observation(&market, 1, revision_before);
}

#[test]
fn next_risk_refresh_integrates_the_post_leverage_mark() {
    let mut market = concentrated_market();
    market.config.ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.config.directional_ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.config.curve_depth_ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.observe_current_risk(1).unwrap();
    let risk_before_leverage = market.risk;
    let mut position = empty_position();
    let quote = market.quote_leverage_swap(MarketAsset::Base, 19_000, 2).unwrap();

    market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::default(),
            0,
            MarketAsset::Base,
            9_500,
            20_000,
            quote.amount_out,
            prepared_leverage_swap(
                &market,
                quote,
                SwapCashPolicy::Borrow {
                    asset: MarketAsset::Base,
                    amount: 9_500,
                },
            ),
            full_fee_credit(&quote),
            0,
            2,
            255,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

    let post_leverage_mark = market.risk.cached_spot_base_price_nad;
    assert_ne!(post_leverage_mark, risk_before_leverage.cached_spot_base_price_nad);
    assert_eq!(market.risk.last_snapshot_slot, 2);
    assert!(market.curve_revision > market.risk_revision);

    let mut expected = market.clone();
    expected.observe_current_risk(3).unwrap();

    let mut stale_pre_leverage_observation = market.clone();
    stale_pre_leverage_observation.risk = risk_before_leverage;
    stale_pre_leverage_observation.observe_current_risk(3).unwrap();
    assert_ne!(
        expected.risk.base_price_ema_nad,
        stale_pre_leverage_observation.risk.base_price_ema_nad
    );

    market.observe_current_risk(3).unwrap();
    assert_eq!(market.risk, expected.risk);
    assert_eq!(market.risk_revision, market.curve_revision);
}

#[test]
fn concentrated_swap_repairs_worsening_stale_hlp_exposure_atomically() {
    let mut market = active_concentrated_hlp_market();
    market.base_hlp_vault.residual_exposure = 1;
    let prepared = SwapRequest {
        current_slot: 1,
        current_unix_timestamp: 0,
        asset_in: MarketAsset::Base,
        reserve_credit: 50_000,
        protocol_fee_bps: 0,
    }
    .prepare(&mut market)
    .unwrap();
    prepared
        .finalize_state(
            &mut market,
            1,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    assert_eq!(market.quote_hlp_vault.residual_exposure, 0);
    market.assert_market_invariants().unwrap();
}

#[test]
fn leverage_execution_rejects_a_tampered_internal_quote() {
    let mut market = concentrated_market();
    let quote = market.quote_leverage_swap(MarketAsset::Base, 50_000, 1).unwrap();

    let mut wrong_direction = quote;
    wrong_direction.asset_in = MarketAsset::Quote.code();
    assert!(market
        .validate_leverage_swap_quote(wrong_direction, MarketAsset::Base, 1)
        .is_err());

    let mut stale_slot = quote;
    stale_slot.quoted_slot = 2;
    assert!(market
        .validate_leverage_swap_quote(stale_slot, MarketAsset::Base, 1)
        .is_err());

    let mut broken_fee_identity = quote;
    broken_fee_identity.reserve_input_credit += 1;
    assert!(market
        .validate_leverage_swap_quote(broken_fee_identity, MarketAsset::Base, 1)
        .is_err());

    market.amm.retain_dynamic_surcharge = !market.amm.retain_dynamic_surcharge;
    assert!(market
        .validate_leverage_swap_quote(quote, MarketAsset::Base, 1)
        .is_err());
}

#[test]
fn distributed_leverage_surcharge_is_all_lp_owned() {
    let mut market = concentrated_market();
    market.amm.retain_dynamic_surcharge = false;
    let quote = market.quote_leverage_swap(MarketAsset::Base, 50_000, 1).unwrap();
    let fee_credit = full_fee_credit(&quote);
    let fee_eligible_ylp_supply = market.base_side.shares.ylp_supply;

    market
        .apply_leverage_lifecycle_transition(
            SwapCashPolicy::Spot,
            MarketAsset::Base,
            quote.amount_in_after_fee,
            quote.amount_out,
            quote.gross_amount_out,
        )
        .unwrap();
    market
        .apply_leverage_swap(
            MarketAsset::Base,
            quote,
            fee_credit,
            0,
            ProtocolAuctionSplit::default(),
            fee_eligible_ylp_supply,
            1,
        )
        .unwrap();

    assert!(quote.fee_breakdown.distributed_surcharge_debit > 0);
    assert_eq!(
        market.base_side.fees.swap_fee_custody_balance,
        quote.fee_breakdown.claimable_fee_debit
    );
    assert_eq!(
        market.base_side.fees.swap_fee_liability + market.base_side.fees.unallocated_swap_fee_liability,
        quote.fee_breakdown.claimable_fee_debit
    );
}

#[test]
fn leverage_operation_advances_the_controller_before_freezing_its_quote() {
    let mut market = concentrated_market();
    market.config.amm.adjustment_threshold_nad = NAD / 100;
    market.config.amm.adjustment_step_nad = NAD / 100;
    market.config.amm.min_adjustment_interval_slots = 1;
    market.amm.price_ema_nad = 2 * NAD;
    market.amm.last_observation_slot = 2;
    market.amm.last_adjustment_slot = 1;
    market.amm.protected_floor_per_share_nad = 0;

    let center_before = market.amm.center_price_nad;
    market.prepare_amm_for_swap(2).unwrap();
    assert!(market.advance_one_amm_controller_target(2).unwrap());
    assert!(market.amm.center_price_nad > center_before);

    let admitted_center = market.amm.center_price_nad;
    let probe_price = market.current_concentrated_spot_price_nad().unwrap().unwrap();
    market.finalize_amm_trade(probe_price, probe_price, 2).unwrap();

    // The lazy controller runs before the leverage quote. Finalization cannot
    // move the curve again inside the same operation.
    assert_eq!(market.amm.center_price_nad, admitted_center);
    assert!(market.amm.retention_target_stale);
}
