use super::*;
use crate::{
    constants::{MARKET_LAYOUT_VERSION, MIN_HALF_LIFE_MS},
    state::{AmmConfig, BorrowPosition, IrmConfig, DEFAULT_DAILY_BORROW_BPS},
};

fn valid_config() -> MarketConfig {
    MarketConfig {
        swap_fee_bps: 30,
        divergence_fee_share_cap_bps: 0,
        volatility_fee_share_cap_bps: 0,
        target_hlp_leverage_bps: 2 * BPS_DENOMINATOR,
        settlement_divergence_bps: BPS_DENOMINATOR,
        ema_half_life_ms: MIN_HALF_LIFE_MS,
        directional_ema_half_life_ms: MIN_HALF_LIFE_MS,
        curve_depth_ema_half_life_ms: MIN_HALF_LIFE_MS,
        max_daily_borrow_bps: DEFAULT_DAILY_BORROW_BPS,
        global_health_contribution_cap_bps: 15_000,
        borrow_market_health_floor_bps: BPS_DENOMINATOR,
        amm: AmmConfig::default(),
        irm: IrmConfig::default(),
        start_time: 0,
    }
}

fn initialized_market() -> BenchmarkMarket {
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let base_side = MarketSide {
        asset_mint: base_mint,
        asset_decimals: 6,
        hlp_mint: Pubkey::new_unique(),
        reserve_vault: Pubkey::new_unique(),
        collateral_vault: Pubkey::new_unique(),
        interest_vault: Pubkey::new_unique(),
        ..MarketSide::default()
    };
    let quote_side = MarketSide {
        asset_mint: quote_mint,
        asset_decimals: 6,
        hlp_mint: Pubkey::new_unique(),
        reserve_vault: Pubkey::new_unique(),
        collateral_vault: Pubkey::new_unique(),
        interest_vault: Pubkey::new_unique(),
        ..MarketSide::default()
    };
    let mut benchmark = BenchmarkMarket::initialize(
        BenchmarkMarketInit {
            srlp_mint: Pubkey::new_unique(),
            base_side,
            quote_side,
            config: valid_config(),
            base_hlp_srlp_vault: Pubkey::new_unique(),
            quote_hlp_srlp_vault: Pubkey::new_unique(),
            params_hash: [7; 32],
            initial_liquidity_authority: Pubkey::new_unique(),
            bootstrap_price_nad: 0,
            launch_fee_progress_offset: 0,
            bump: 255,
        },
        BenchmarkClock {
            slot: 0,
            unix_timestamp: 0,
        },
    )
    .unwrap();
    benchmark.market_mut().add_liquidity(1_000_000, 2_000_000).unwrap();
    benchmark
        .advance_to(BenchmarkClock {
            slot: 1,
            unix_timestamp: 1,
        })
        .unwrap();
    benchmark
}

fn market_bytes(market: &Market) -> Vec<u8> {
    let mut bytes = Vec::new();
    market.try_serialize(&mut bytes).unwrap();
    bytes
}

fn position_bytes(position: &BorrowPosition) -> Vec<u8> {
    let mut bytes = Vec::new();
    position.try_serialize(&mut bytes).unwrap();
    bytes
}

fn initialized_keyed_market() -> (BenchmarkMarket, Pubkey) {
    let benchmark = initialized_market();
    let market_key = Pubkey::new_unique();
    let keyed = BenchmarkMarket::from_market_state_at(
        market_key,
        clone_market(benchmark.market()).unwrap(),
        benchmark.clock(),
    )
    .unwrap();
    (keyed, market_key)
}

#[test]
fn borrow_position_replay_rejects_default_owner_and_market() {
    let (market, market_key) = initialized_keyed_market();
    let position_id = Pubkey::new_unique();
    assert!(BenchmarkBorrowPosition::initialize(Pubkey::default(), market_key, position_id, 1).is_err());
    assert!(BenchmarkBorrowPosition::initialize(Pubkey::new_unique(), Pubkey::default(), position_id, 1).is_err());

    let mut decoded = BorrowPosition::default();
    decoded.initialize(Pubkey::default(), market_key, position_id, 1);
    assert!(BenchmarkBorrowPosition::from_position_state(decoded.clone()).is_err());
    let invalid = BenchmarkBorrowPosition {
        position: Box::new(decoded),
    };
    assert!(invalid.checkpoint(&market).is_err());

    assert!(BenchmarkBorrowPosition::initialize(Pubkey::new_unique(), market_key, position_id, 1).is_ok());
}

fn borrow_position_with_collateral(collateral_asset: MarketAsset, collateral_amount: u64) -> BorrowPosition {
    let mut position = BorrowPosition {
        active_liquidation_session: Pubkey::default(),

        owner: Pubkey::new_unique(),
        market: Pubkey::new_unique(),
        position_id: Pubkey::new_unique(),
        base_collateral: 0,
        quote_collateral: 0,
        global_health_base_contribution_for_quote_debt: 0,
        global_health_quote_contribution_for_base_debt: 0,
        base_liquidation_cf_bps: 0,
        quote_liquidation_cf_bps: 0,
        base_referral_partner: Pubkey::default(),
        quote_referral_partner: Pubkey::default(),
        base_referral_interest_share_bps: 0,
        quote_referral_interest_share_bps: 0,
        fixed_base_shares: 0,
        fixed_quote_shares: 0,
        fixed_base_principal: 0,
        fixed_quote_principal: 0,
        auction_debt_asset: u8::MAX,
        auction_start_time: 0,
        auction_start_price_nad: 0,
        auction_floor_price_nad: 0,
        bump: 255,
    };
    match collateral_asset {
        MarketAsset::Base => position.base_collateral = collateral_amount,
        MarketAsset::Quote => position.quote_collateral = collateral_amount,
    }
    position
}

fn borrowed_liquidation_fixture(collateral_after: u64) -> (BenchmarkMarket, BenchmarkBorrowPosition, u64) {
    let (mut benchmark, market_key) = initialized_keyed_market();
    let mut position =
        BenchmarkBorrowPosition::initialize(Pubkey::new_unique(), market_key, Pubkey::new_unique(), 251).unwrap();
    benchmark
        .execute_deposit_collateral(
            &mut position,
            BenchmarkDepositCollateralRequest {
                clock: BenchmarkClock {
                    slot: 2,
                    unix_timestamp: 2,
                },
                collateral_asset: MarketAsset::Quote,
                collateral_credit: 300_000,
            },
        )
        .unwrap();
    let capacity = benchmark
        .preview_borrow_capacity(MarketAsset::Quote, 300_000, None)
        .unwrap()
        .max_borrow_amount;
    let borrowed = capacity / 2;
    benchmark
        .execute_borrow(
            &mut position,
            BenchmarkBorrowRequest {
                clock: BenchmarkClock {
                    slot: 3,
                    unix_timestamp: 3,
                },
                debt_asset: MarketAsset::Base,
                borrow_amount: borrowed,
                recipient_credit: borrowed,
                min_recipient_credit: 0,
                min_liquidation_cf_bps: 0,
                global_reduce_only: false,
                referral_partner: Pubkey::default(),
                referral_interest_share_bps: 0,
                referral_interest_share_cap_bps: 0,
            },
        )
        .unwrap();
    // Model an adverse collateral-price path by reducing only this
    // position's claim. The vault retains the historical gross custody;
    // unrelated atoms are inert donations at this unit-test boundary.
    position.position.quote_collateral = collateral_after;
    assert!(benchmark
        .market()
        .is_position_liquidatable(position.position(), MarketAsset::Base)
        .unwrap());
    (benchmark, position, borrowed)
}

fn exact_transfer(amount: u64) -> BenchmarkTokenTransferOutcome {
    BenchmarkTokenTransferOutcome {
        source_debit: amount,
        destination_credit: amount,
    }
}

#[test]
fn explicit_clock_advance_matches_native_update_order() {
    let mut benchmark = initialized_market();
    let mut native = clone_market(benchmark.market()).unwrap();
    let clock = BenchmarkClock {
        slot: 10_000,
        unix_timestamp: 500,
    };

    native.accrue_interest_to_slot(clock.slot).unwrap();
    native.advance_amm_clock(clock.slot).unwrap();
    native.checkpoint_hlp_vaults().unwrap();
    native.refresh_risk_at_slot(clock.slot).unwrap();
    benchmark.advance_to(clock).unwrap();

    assert_eq!(benchmark.clock(), clock);
    assert_eq!(market_bytes(benchmark.market()), market_bytes(&native));
    assert_eq!(benchmark.market().version, MARKET_LAYOUT_VERSION);
    // With no debt, the coverage controller holds its configured rate.
    assert_eq!(
        benchmark.market().debt.base_borrow_apr_nad,
        benchmark.market().config.irm.initial_apr_nad as u128
    );
}

#[test]
fn pure_preview_and_commit_match_the_native_swap_pipeline_exactly() {
    let mut benchmark = initialized_market();
    let request = BenchmarkSwapRequest {
        asset_in: MarketAsset::Base,
        reserve_credit: 10_000,
        protocol_fee_bps: 1_000,
        protocol_auction_split: ProtocolAuctionSplit::default(),
    };
    let before = market_bytes(benchmark.market());
    let preview = benchmark.preview_swap(request).unwrap();
    assert_eq!(market_bytes(benchmark.market()), before);

    let mut native = clone_market(benchmark.market()).unwrap();
    let mut prepared = SwapRequest {
        current_slot: benchmark.clock().slot,
        current_unix_timestamp: benchmark.clock().unix_timestamp,
        asset_in: request.asset_in,
        reserve_credit: request.reserve_credit,
        protocol_fee_bps: request.protocol_fee_bps,
    }
    .prepare(&mut native)
    .unwrap();
    let native_quote = prepared.quote;
    let native_fee_supply = prepared.fee_eligible_srlp_supply;
    let native_interest_eligibility = prepared.interest_eligibility;
    let native_finalized = prepared
        .finalize_state(
            &mut native,
            benchmark.clock().slot,
            request.protocol_fee_bps,
            request.protocol_auction_split,
        )
        .unwrap();

    assert_eq!(preview.quote, native_quote);
    assert_eq!(preview.base_rebalance, native_finalized.base_rebalance);
    assert_eq!(preview.quote_rebalance, native_finalized.quote_rebalance);
    assert_eq!(preview.fee_eligible_srlp_supply, native_fee_supply);
    assert_eq!(preview.interest_eligibility, native_interest_eligibility);

    let execution = benchmark.execute_swap(request).unwrap();
    assert_eq!(execution, preview);
    assert_eq!(market_bytes(benchmark.market()), market_bytes(&native));
}

#[test]
fn borrow_capacity_bound_is_accepted_and_the_next_atom_is_rejected() {
    let benchmark = initialized_market();
    let collateral_asset = MarketAsset::Quote;
    let collateral_amount = 200_000;
    let capacity = benchmark
        .preview_borrow_capacity(collateral_asset, collateral_amount, None)
        .unwrap();
    assert!(capacity.max_borrow_amount > 0);

    let mut accepted = benchmark.try_fork().unwrap();
    let mut accepted_position = borrow_position_with_collateral(collateral_asset, collateral_amount);
    accepted
        .transact(|market, clock| {
            market.borrow(
                &mut accepted_position,
                collateral_asset.opposite(),
                capacity.max_borrow_amount,
                0,
                clock.slot,
            )
        })
        .unwrap();

    let mut rejected = benchmark.try_fork().unwrap();
    let mut rejected_position = borrow_position_with_collateral(collateral_asset, collateral_amount);
    assert!(rejected
        .transact(|market, clock| {
            market.borrow(
                &mut rejected_position,
                collateral_asset.opposite(),
                capacity.max_borrow_amount + 1,
                0,
                clock.slot,
            )
        })
        .is_err());
    assert_eq!(market_bytes(rejected.market()), market_bytes(benchmark.market()));
}

fn transfer_fee(bps: u16) -> TransferFee {
    TransferFee {
        epoch: 0_u64.into(),
        maximum_fee: u64::MAX.into(),
        transfer_fee_basis_points: bps.into(),
    }
}

#[test]
fn borrow_capacity_values_collateral_at_the_worse_scheduled_fee() {
    let fee_free = initialized_market();
    let mut benchmark = initialized_market();
    benchmark.set_mint_transfer_fee(
        MarketAsset::Quote,
        BenchmarkMintTransferFee {
            effective: Some(transfer_fee(1_000)),
            pending: Some(transfer_fee(3_000)),
            pending_liquidation_eligible: false,
        },
    );
    let collateral_amount = 200_000;
    let capacity = benchmark
        .preview_borrow_capacity(MarketAsset::Quote, collateral_amount, None)
        .unwrap();
    let fee_free_capacity = fee_free
        .preview_borrow_capacity(MarketAsset::Quote, collateral_amount, None)
        .unwrap();
    assert!(capacity.max_borrow_amount > 0);
    assert!(capacity.max_borrow_amount < fee_free_capacity.max_borrow_amount);

    let admission = benchmark.mint_transfer_fee(MarketAsset::Quote).admission();
    let mut accepted = benchmark.try_fork().unwrap();
    let mut accepted_position = borrow_position_with_collateral(MarketAsset::Quote, collateral_amount);
    accepted
        .transact(|market, clock| {
            market.borrow_with_collateral_fee(
                &mut accepted_position,
                MarketAsset::Base,
                capacity.max_borrow_amount,
                admission,
                0,
                clock.slot,
            )
        })
        .unwrap();
    let mut rejected = benchmark.try_fork().unwrap();
    let mut rejected_position = borrow_position_with_collateral(MarketAsset::Quote, collateral_amount);
    assert!(rejected
        .transact(|market, clock| {
            market.borrow_with_collateral_fee(
                &mut rejected_position,
                MarketAsset::Base,
                capacity.max_borrow_amount + 1,
                admission,
                0,
                clock.slot,
            )
        })
        .is_err());
}

#[test]
fn confiscatory_fee_returns_zero_borrow_capacity_instead_of_error() {
    let mut benchmark = initialized_market();
    benchmark.set_mint_transfer_fee(
        MarketAsset::Quote,
        BenchmarkMintTransferFee {
            effective: Some(transfer_fee(10_000)),
            pending: None,
            pending_liquidation_eligible: false,
        },
    );
    let capacity = benchmark
        .preview_borrow_capacity(MarketAsset::Quote, 200_000, None)
        .unwrap();
    assert_eq!(capacity.collateral_value_nad, 0);
    assert_eq!(capacity.max_borrow_amount, 0);
    assert_eq!(capacity.projected_borrow_amount, 0);
    let requested = benchmark
        .preview_borrow_capacity(MarketAsset::Quote, 200_000, Some(100))
        .unwrap();
    assert_eq!(requested.max_borrow_amount, 0);
    assert_eq!(requested.projected_borrow_amount, 100);
    assert_eq!(requested.projected_health_bps, 0);
}

#[test]
fn effective_collateral_fee_makes_a_healthy_position_liquidatable() {
    let (mut benchmark, market_key) = initialized_keyed_market();
    let mut position =
        BenchmarkBorrowPosition::initialize(Pubkey::new_unique(), market_key, Pubkey::new_unique(), 249).unwrap();
    benchmark
        .execute_deposit_collateral(
            &mut position,
            BenchmarkDepositCollateralRequest {
                clock: BenchmarkClock {
                    slot: 2,
                    unix_timestamp: 2,
                },
                collateral_asset: MarketAsset::Quote,
                collateral_credit: 300_000,
            },
        )
        .unwrap();
    let borrowed = benchmark
        .preview_borrow_capacity(MarketAsset::Quote, 300_000, None)
        .unwrap()
        .max_borrow_amount
        / 2;
    benchmark
        .execute_borrow(
            &mut position,
            BenchmarkBorrowRequest {
                clock: BenchmarkClock {
                    slot: 3,
                    unix_timestamp: 3,
                },
                debt_asset: MarketAsset::Base,
                borrow_amount: borrowed,
                recipient_credit: borrowed,
                min_recipient_credit: 0,
                min_liquidation_cf_bps: 0,
                global_reduce_only: false,
                referral_partner: Pubkey::default(),
                referral_interest_share_bps: 0,
                referral_interest_share_cap_bps: 0,
            },
        )
        .unwrap();
    let start = BenchmarkStartLiquidationAuctionRequest {
        clock: BenchmarkClock {
            slot: 4,
            unix_timestamp: 4,
        },
        debt_asset: MarketAsset::Base,
    };
    assert!(benchmark
        .try_fork()
        .unwrap()
        .execute_start_liquidation_auction(&mut position.try_fork().unwrap(), start)
        .is_err());

    // The first notice epoch remains a borrower repayment window.
    benchmark.set_mint_transfer_fee(
        MarketAsset::Quote,
        BenchmarkMintTransferFee {
            effective: None,
            pending: Some(transfer_fee(10_000)),
            pending_liquidation_eligible: false,
        },
    );
    assert!(benchmark
        .try_fork()
        .unwrap()
        .execute_start_liquidation_auction(&mut position.try_fork().unwrap(), start)
        .is_err());

    benchmark.set_mint_transfer_fee(
        MarketAsset::Quote,
        BenchmarkMintTransferFee {
            effective: None,
            pending: Some(transfer_fee(10_000)),
            pending_liquidation_eligible: true,
        },
    );
    benchmark
        .try_fork()
        .unwrap()
        .execute_start_liquidation_auction(&mut position.try_fork().unwrap(), start)
        .unwrap();

    benchmark.set_mint_transfer_fee(
        MarketAsset::Quote,
        BenchmarkMintTransferFee {
            effective: Some(transfer_fee(10_000)),
            pending: None,
            pending_liquidation_eligible: false,
        },
    );
    benchmark
        .execute_start_liquidation_auction(&mut position, start)
        .unwrap();
}

#[test]
fn existing_position_capacity_decomposes_native_bounds_without_mutation() {
    let (mut benchmark, market_key) = initialized_keyed_market();
    let mut position =
        BenchmarkBorrowPosition::initialize(Pubkey::new_unique(), market_key, Pubkey::new_unique(), 250).unwrap();
    benchmark
        .execute_deposit_collateral(
            &mut position,
            BenchmarkDepositCollateralRequest {
                clock: BenchmarkClock {
                    slot: 2,
                    unix_timestamp: 2,
                },
                collateral_asset: MarketAsset::Quote,
                collateral_credit: 300_000,
            },
        )
        .unwrap();
    let initial = benchmark
        .preview_existing_borrow_capacity(
            &position,
            MarketAsset::Base,
            BenchmarkClock {
                slot: 3,
                unix_timestamp: 3,
            },
            false,
        )
        .unwrap();
    assert!(initial.actionable_max_additional > 2);
    let first_borrow = initial.actionable_max_additional / 3;
    benchmark
        .execute_borrow(
            &mut position,
            BenchmarkBorrowRequest {
                clock: BenchmarkClock {
                    slot: 3,
                    unix_timestamp: 3,
                },
                debt_asset: MarketAsset::Base,
                borrow_amount: first_borrow,
                recipient_credit: first_borrow,
                min_recipient_credit: 0,
                min_liquidation_cf_bps: 0,
                global_reduce_only: false,
                referral_partner: Pubkey::default(),
                referral_interest_share_bps: 0,
                referral_interest_share_cap_bps: 0,
            },
        )
        .unwrap();

    let market_before = market_bytes(benchmark.market());
    let position_before = position_bytes(position.position());
    let clock_before = benchmark.clock();
    let clock = BenchmarkClock {
        slot: 4,
        unix_timestamp: 4,
    };
    let capacity = benchmark
        .preview_existing_borrow_capacity(&position, MarketAsset::Base, clock, false)
        .unwrap();
    assert!(capacity.debt_before > 0);
    assert!(capacity.gross_admissible_max_debt >= capacity.actionable_max_debt);
    assert_eq!(
        capacity.actionable_max_additional,
        capacity
            .underwriting_max_additional
            .min(capacity.global_health_floor_max_additional)
            .min(capacity.cash_max_additional)
            .min(capacity.daily_bucket_max_additional)
    );
    assert!(!capacity.limiting_constraints.is_empty());
    assert_eq!(market_bytes(benchmark.market()), market_before);
    assert_eq!(position_bytes(position.position()), position_before);
    assert_eq!(benchmark.clock(), clock_before);

    if capacity.actionable_max_additional > 0 {
        let mut accepted_market = benchmark.try_fork().unwrap();
        let mut accepted_position = position.try_fork().unwrap();
        accepted_market
            .execute_borrow(
                &mut accepted_position,
                BenchmarkBorrowRequest {
                    clock,
                    debt_asset: MarketAsset::Base,
                    borrow_amount: capacity.actionable_max_additional,
                    recipient_credit: capacity.actionable_max_additional,
                    min_recipient_credit: 0,
                    min_liquidation_cf_bps: 0,
                    global_reduce_only: false,
                    referral_partner: Pubkey::default(),
                    referral_interest_share_bps: 0,
                    referral_interest_share_cap_bps: 0,
                },
            )
            .unwrap();

        if capacity.actionable_max_additional < u64::MAX {
            let mut rejected_market = benchmark.try_fork().unwrap();
            let mut rejected_position = position.try_fork().unwrap();
            let next = capacity.actionable_max_additional + 1;
            assert!(rejected_market
                .execute_borrow(
                    &mut rejected_position,
                    BenchmarkBorrowRequest {
                        clock,
                        debt_asset: MarketAsset::Base,
                        borrow_amount: next,
                        recipient_credit: next,
                        min_recipient_credit: 0,
                        min_liquidation_cf_bps: 0,
                        global_reduce_only: false,
                        referral_partner: Pubkey::default(),
                        referral_interest_share_bps: 0,
                        referral_interest_share_cap_bps: 0,
                    },
                )
                .is_err());
        }
    }
}

#[test]
fn existing_position_capacity_reduce_only_is_exact_zero_actionable() {
    let (mut benchmark, market_key) = initialized_keyed_market();
    let mut position =
        BenchmarkBorrowPosition::initialize(Pubkey::new_unique(), market_key, Pubkey::new_unique(), 249).unwrap();
    benchmark
        .execute_deposit_collateral(
            &mut position,
            BenchmarkDepositCollateralRequest {
                clock: BenchmarkClock {
                    slot: 2,
                    unix_timestamp: 2,
                },
                collateral_asset: MarketAsset::Quote,
                collateral_credit: 300_000,
            },
        )
        .unwrap();
    let capacity = benchmark
        .preview_existing_borrow_capacity(
            &position,
            MarketAsset::Base,
            BenchmarkClock {
                slot: 3,
                unix_timestamp: 3,
            },
            true,
        )
        .unwrap();
    assert_eq!(capacity.actionable_max_additional, 0);
    assert_eq!(capacity.actionable_max_debt, capacity.debt_before);
    assert!(capacity.gross_admissible_max_debt > capacity.debt_before);
    assert!(capacity
        .limiting_constraints
        .contains(&BenchmarkBorrowCapacityConstraint::ReduceOnly));
}

#[test]
fn existing_position_target_solver_preserves_post_debt_atoms_across_share_rounding() {
    let (mut benchmark, market_key) = initialized_keyed_market();
    let mut position =
        BenchmarkBorrowPosition::initialize(Pubkey::new_unique(), market_key, Pubkey::new_unique(), 248).unwrap();
    benchmark
        .execute_deposit_collateral(
            &mut position,
            BenchmarkDepositCollateralRequest {
                clock: BenchmarkClock {
                    slot: 2,
                    unix_timestamp: 2,
                },
                collateral_asset: MarketAsset::Quote,
                collateral_credit: 300_000,
            },
        )
        .unwrap();
    benchmark
        .execute_borrow(
            &mut position,
            BenchmarkBorrowRequest {
                clock: BenchmarkClock {
                    slot: 3,
                    unix_timestamp: 3,
                },
                debt_asset: MarketAsset::Base,
                borrow_amount: 10_001,
                recipient_credit: 10_001,
                min_recipient_credit: 0,
                min_liquidation_cf_bps: 0,
                global_reduce_only: false,
                referral_partner: Pubkey::default(),
                referral_interest_share_bps: 0,
                referral_interest_share_cap_bps: 0,
            },
        )
        .unwrap();
    // One new debt share is worth either one or two debt atoms at this
    // non-NAD index. The raw transfer request and post-debt delta therefore
    // cannot be treated as interchangeable units.
    benchmark.market_mut().debt.base_borrow_index_nad = (NAD as u128) * 3 / 2;
    benchmark.market_mut().config.borrow_market_health_floor_bps = 9_000;
    let clock = BenchmarkClock {
        slot: 3,
        unix_timestamp: 3,
    };
    let capacity = benchmark
        .preview_existing_borrow_capacity(&position, MarketAsset::Base, clock, false)
        .unwrap();
    let target = capacity.debt_before + 2;
    let solved = benchmark
        .preview_existing_borrow_request_for_target(&position, MarketAsset::Base, clock, false, target)
        .unwrap();
    assert_eq!(solved.borrow_request, 1);
    assert_eq!(solved.projected_debt, target);
    assert_eq!(solved.target_debt_gap, 0);
    assert_ne!(
        u128::from(solved.borrow_request),
        solved.projected_debt - capacity.debt_before
    );

    let unreachable = benchmark
        .preview_existing_borrow_request_for_target(
            &position,
            MarketAsset::Base,
            clock,
            false,
            capacity.debt_before + 1,
        )
        .unwrap();
    assert_eq!(unreachable.borrow_request, 0);
    assert_eq!(unreachable.target_debt_gap, 1);

    let receipt = benchmark
        .execute_borrow(
            &mut position,
            BenchmarkBorrowRequest {
                clock,
                debt_asset: MarketAsset::Base,
                borrow_amount: solved.borrow_request,
                recipient_credit: solved.borrow_request,
                min_recipient_credit: 0,
                min_liquidation_cf_bps: 0,
                global_reduce_only: false,
                referral_partner: Pubkey::default(),
                referral_interest_share_bps: 0,
                referral_interest_share_cap_bps: 0,
            },
        )
        .unwrap();
    assert_eq!(receipt.position_after.fixed_base_debt, solved.projected_debt);
}

#[test]
fn balanced_srlp_wrappers_match_native_transition_order_exactly() {
    let mut benchmark = initialized_market();
    let mut native = clone_market(benchmark.market()).unwrap();
    let add_clock = BenchmarkClock {
        slot: 20,
        unix_timestamp: 20,
    };
    let add_request = BenchmarkAddSrlpRequest {
        clock: add_clock,
        owner: Pubkey::new_unique(),
        base_reserve_credit: 100_000,
        quote_reserve_credit: 200_000,
        min_srlp_amount: 100_000,
        global_reduce_only: false,
    };

    advance_market_to_slot(&mut native, add_clock.slot).unwrap();
    carry_forward_all_srlp_revenue(&mut native).unwrap();
    let native_add = native
        .add_liquidity(add_request.base_reserve_credit, add_request.quote_reserve_credit)
        .unwrap();
    native.finalize_amm_transition_and_observe_risk(add_clock.slot).unwrap();

    let add = benchmark.execute_add_srlp(add_request).unwrap();
    assert_eq!(add.receipt.srlp_amount, native_add.srlp_amount);
    assert_eq!(add.receipt.srlp_supply, native_add.srlp_supply);
    assert_eq!(add.receipt.base_reserve_amount, native_add.base_reserve_credit);
    assert_eq!(add.receipt.quote_reserve_amount, native_add.quote_reserve_credit);
    assert_eq!(add.cash.base.reserve_vault_credit, native_add.base_reserve_credit);
    assert_eq!(add.cash.quote.reserve_vault_credit, native_add.quote_reserve_credit);
    assert_eq!(add.market_after, benchmark.checkpoint().unwrap());
    assert_eq!(market_bytes(benchmark.market()), market_bytes(&native));

    let remove_clock = BenchmarkClock {
        slot: 21,
        unix_timestamp: 21,
    };
    let srlp_amount = native_add.srlp_amount / 2;
    advance_market_to_slot(&mut native, remove_clock.slot).unwrap();
    carry_forward_all_srlp_revenue(&mut native).unwrap();
    let native_remove = native.remove_liquidity(srlp_amount).unwrap();
    native
        .finalize_amm_transition_and_observe_risk(remove_clock.slot)
        .unwrap();
    native.assert_market_health().unwrap();

    let remove = benchmark
        .execute_remove_srlp(BenchmarkRemoveSrlpRequest {
            clock: remove_clock,
            srlp_amount,
            owner_srlp_balance_before: native_add.srlp_amount,
            base_recipient_credit: native_remove.base_amount_out,
            quote_recipient_credit: native_remove.quote_amount_out,
            min_base_recipient_credit: native_remove.base_amount_out,
            min_quote_recipient_credit: native_remove.quote_amount_out,
        })
        .unwrap();
    assert_eq!(remove.receipt.srlp_amount, native_remove.srlp_amount);
    assert_eq!(remove.receipt.srlp_supply, native_remove.srlp_supply);
    assert_eq!(remove.cash.base.reserve_vault_debit, native_remove.base_amount_out);
    assert_eq!(remove.cash.quote.reserve_vault_debit, native_remove.quote_amount_out);
    assert_eq!(remove.cash.base.recipient_credit, native_remove.base_amount_out);
    assert_eq!(remove.cash.quote.recipient_credit, native_remove.quote_amount_out);
    assert_eq!(market_bytes(benchmark.market()), market_bytes(&native));
}

#[test]
fn position_wrappers_match_native_deposit_borrow_and_repay() {
    let (mut benchmark, market_key) = initialized_keyed_market();
    benchmark.market.protocol_interest_markup_bps = 2_000;
    let owner = Pubkey::new_unique();
    let position_id = Pubkey::new_unique();
    let mut position = BenchmarkBorrowPosition::initialize(owner, market_key, position_id, 254).unwrap();
    let mut referral = BenchmarkReferralOwnedState::initialize(
        &benchmark,
        MarketAsset::Base,
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        5_000,
        true,
    )
    .unwrap();
    let referral_partner = referral.partner_key;
    let mut native_market = clone_market(benchmark.market()).unwrap();
    let mut native_position = BenchmarkBorrowPosition::initialize(owner, market_key, position_id, 254)
        .unwrap()
        .into_position();

    let deposit_clock = BenchmarkClock {
        slot: 2,
        unix_timestamp: 2,
    };
    advance_market_to_slot(&mut native_market, deposit_clock.slot).unwrap();
    native_market.assert_started_at(deposit_clock.unix_timestamp).unwrap();
    let native_deposit = native_market
        .deposit_collateral(&mut native_position, MarketAsset::Quote, 300_000)
        .unwrap();
    let deposit = benchmark
        .execute_deposit_collateral(
            &mut position,
            BenchmarkDepositCollateralRequest {
                clock: deposit_clock,
                collateral_asset: MarketAsset::Quote,
                collateral_credit: 300_000,
            },
        )
        .unwrap();
    assert_eq!(deposit.market.receipt, native_deposit);
    assert_eq!(deposit.market.cash.quote.collateral_vault_credit, 300_000);
    assert_eq!(market_bytes(benchmark.market()), market_bytes(&native_market));
    assert_eq!(position_bytes(position.position()), position_bytes(&native_position));

    let capacity = benchmark
        .preview_borrow_capacity(MarketAsset::Quote, 300_000, None)
        .unwrap();
    assert!(capacity.max_borrow_amount >= 2);
    let borrow_amount = capacity.max_borrow_amount / 2;
    let borrow_clock = BenchmarkClock {
        slot: 3,
        unix_timestamp: 3,
    };
    advance_market_to_slot(&mut native_market, borrow_clock.slot).unwrap();
    native_market.assert_started_at(borrow_clock.unix_timestamp).unwrap();
    native_position.set_referral_binding(MarketAsset::Base, referral_partner, 5_000);
    let native_borrow = native_market
        .borrow(
            &mut native_position,
            MarketAsset::Base,
            borrow_amount,
            0,
            borrow_clock.slot,
        )
        .unwrap();
    native_market.finalize_amm_transition(borrow_clock.slot).unwrap();
    native_market.refresh_risk_at_slot(borrow_clock.slot).unwrap();
    let borrow = benchmark
        .execute_borrow(
            &mut position,
            BenchmarkBorrowRequest {
                clock: borrow_clock,
                debt_asset: MarketAsset::Base,
                borrow_amount,
                recipient_credit: borrow_amount,
                min_recipient_credit: borrow_amount,
                min_liquidation_cf_bps: 0,
                global_reduce_only: false,
                referral_partner,
                referral_interest_share_bps: 5_000,
                referral_interest_share_cap_bps: BPS_DENOMINATOR,
            },
        )
        .unwrap();
    assert_eq!(borrow.market.receipt, native_borrow);
    assert_eq!(borrow.market.cash.base.reserve_vault_debit, borrow_amount);
    assert_eq!(borrow.market.cash.base.recipient_credit, borrow_amount);
    assert_eq!(market_bytes(benchmark.market()), market_bytes(&native_market));
    assert_eq!(position_bytes(position.position()), position_bytes(&native_position));

    let repay_clock = BenchmarkClock {
        slot: 10_000_003,
        unix_timestamp: 10_000_003,
    };
    advance_market_to_slot(&mut native_market, repay_clock.slot).unwrap();
    native_market.assert_started_at(repay_clock.unix_timestamp).unwrap();
    let repayment = native_market
        .fixed_repayment_for_max(&native_position, MarketAsset::Base, u64::MAX)
        .unwrap();
    let native_repay = native_market
        .repay(&mut native_position, MarketAsset::Base, repayment.cash_repaid)
        .unwrap();
    let protocol = native_market
        .quote_interest_revenue_credit(MarketAsset::Base, false, native_repay.interest_paid)
        .unwrap()
        .protocol;
    let expected_referral = ReferralInterestQuote::with_protocol_revenue(
        native_repay.interest_paid,
        native_repay.interest_paid,
        protocol,
        Some(5_000),
    )
    .unwrap()
    .referral_amount;
    assert!(expected_referral > 0);
    if native_repay.interest_paid > 0 {
        native_market
            .record_receivable_income_credit(
                MarketAsset::Base,
                false,
                native_repay.interest_paid,
                protocol,
                ProtocolAuctionSplit::default(),
                expected_referral,
            )
            .unwrap();
    }
    native_market.finalize_amm_transition(repay_clock.slot).unwrap();
    native_market.refresh_risk_at_slot(repay_clock.slot).unwrap();

    let market_before = market_bytes(benchmark.market());
    let position_before = position_bytes(position.position());
    let clock_before = benchmark.clock();
    let repay_request = BenchmarkRepayRequest {
        clock: repay_clock,
        max_repay_credit: u64::MAX,
        interest_vault_credit: native_repay.interest_paid,
        protocol_interest_fee_bps: 2_000,
        protocol_auction_split: ProtocolAuctionSplit::default(),
        referral_interest_amount: expected_referral,
        debt_asset: MarketAsset::Base,
    };
    assert!(benchmark.execute_repay(&mut position, None, repay_request).is_err());
    let mut wrong_referral = BenchmarkReferralOwnedState::initialize(
        &benchmark,
        MarketAsset::Base,
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        5_000,
        true,
    )
    .unwrap();
    assert!(benchmark
        .execute_repay(&mut position, Some(&mut wrong_referral), repay_request)
        .is_err());
    assert_eq!(wrong_referral.checkpoint().accrued_amount, 0);
    assert!(benchmark
        .execute_repay(
            &mut position,
            Some(&mut referral),
            BenchmarkRepayRequest {
                referral_interest_amount: expected_referral + 1,
                ..repay_request
            },
        )
        .is_err());
    assert_eq!(referral.checkpoint().accrued_amount, 0);
    assert_eq!(market_bytes(benchmark.market()), market_before);
    assert_eq!(position_bytes(position.position()), position_before);
    assert_eq!(benchmark.clock(), clock_before);

    let repay = benchmark
        .execute_repay(&mut position, Some(&mut referral), repay_request)
        .unwrap();
    assert_eq!(repay.market.receipt, native_repay);
    assert_eq!(repay.market.cash.base.reserve_vault_credit, native_repay.cash_repaid);
    assert_eq!(repay.market.cash.base.reserve_vault_debit, native_repay.interest_paid);
    assert_eq!(repay.market.cash.base.interest_vault_credit, native_repay.interest_paid);
    assert_eq!(market_bytes(benchmark.market()), market_bytes(&native_market));
    assert_eq!(position_bytes(position.position()), position_bytes(&native_position));
    assert_eq!(repay.position_after, position.checkpoint(&benchmark).unwrap());
    assert_eq!(referral.checkpoint().accrued_amount, expected_referral);
    let snapshot = referral.try_fork().unwrap();
    let restored = BenchmarkReferralOwnedState::from_account_state(
        &benchmark,
        MarketAsset::Base,
        snapshot.partner_key,
        *snapshot.partner,
        snapshot.accrual_key,
        *snapshot.accrual,
    )
    .unwrap();
    assert_eq!(restored.checkpoint(), referral.checkpoint());
}

#[test]
fn failed_position_transition_rolls_back_market_position_and_clock() {
    let (mut benchmark, market_key) = initialized_keyed_market();
    let owner = Pubkey::new_unique();
    let mut position = BenchmarkBorrowPosition::initialize(owner, market_key, Pubkey::new_unique(), 253).unwrap();
    benchmark
        .execute_deposit_collateral(
            &mut position,
            BenchmarkDepositCollateralRequest {
                clock: BenchmarkClock {
                    slot: 2,
                    unix_timestamp: 2,
                },
                collateral_asset: MarketAsset::Quote,
                collateral_credit: 300_000,
            },
        )
        .unwrap();
    let market_before = market_bytes(benchmark.market());
    let position_before = position_bytes(position.position());
    let clock_before = benchmark.clock();
    let impossible_amount = benchmark.market().base_side.reserves.cash_reserve + 1;

    assert!(benchmark
        .execute_borrow(
            &mut position,
            BenchmarkBorrowRequest {
                clock: BenchmarkClock {
                    slot: 50_000,
                    unix_timestamp: 50_000,
                },
                debt_asset: MarketAsset::Base,
                borrow_amount: impossible_amount,
                recipient_credit: impossible_amount,
                min_recipient_credit: impossible_amount,
                min_liquidation_cf_bps: 0,
                global_reduce_only: false,
                referral_partner: Pubkey::new_unique(),
                referral_interest_share_bps: 100,
                referral_interest_share_cap_bps: 100,
            },
        )
        .is_err());
    assert_eq!(benchmark.clock(), clock_before);
    assert_eq!(market_bytes(benchmark.market()), market_before);
    assert_eq!(position_bytes(position.position()), position_before);
}

#[test]
fn leverage_and_hlp_external_state_are_fully_owned() {
    assert_eq!(
        BenchmarkMarket::leverage_external_requirements().requirements,
        LEVERAGE_EXTERNAL_REQUIREMENTS
    );
    assert_eq!(
        BenchmarkMarket::hlp_external_requirements().requirements,
        HLP_EXTERNAL_REQUIREMENTS
    );
    assert!(BenchmarkMarket::leverage_external_requirements()
        .requirements
        .is_empty());
    assert!(BenchmarkMarket::hlp_external_requirements().requirements.is_empty());
}

#[test]
fn owned_hlp_entry_matches_native_market_and_account_ordering() {
    let (mut benchmark, market_key) = initialized_keyed_market();
    let owner = Pubkey::new_unique();
    let mut external = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Base, owner, owner).unwrap();
    let mut expected_market = clone_market(benchmark.market()).unwrap();
    let mut expected_external = external.try_fork().unwrap();
    let request = BenchmarkHlpEntryRequest {
        funding: BenchmarkHlpFundingSettlement::default(),
        clock: BenchmarkClock {
            slot: 2,
            unix_timestamp: 2,
        },
        target_transfer: BenchmarkTokenTransferOutcome {
            source_debit: 100_100,
            destination_credit: 100_000,
        },
        min_hlp_amount: 1,
        global_reduce_only: false,
    };

    expected_market.accrue_interest_to_slot(request.clock.slot).unwrap();
    reconcile_live_hlp_supply(
        &mut expected_market,
        MarketAsset::Base,
        expected_external.hlp_mint_supply,
    )
    .unwrap();
    expected_market.advance_amm_clock(request.clock.slot).unwrap();
    expected_market.checkpoint_hlp_vaults().unwrap();
    expected_market.assert_hlp_entry_available(MarketAsset::Base).unwrap();
    expected_market.observe_current_risk(request.clock.slot).unwrap();
    let expected_receipt = expected_market
        .deposit_single_sided(MarketAsset::Base, request.target_transfer.destination_credit, 1)
        .unwrap();
    expected_market
        .finalize_amm_transition_and_observe_risk(request.clock.slot)
        .unwrap();
    let (base_swap, base_interest) = expected_market.hlp_yield_growth_indexes(MarketAsset::Base, MarketAsset::Base);
    let (quote_swap, quote_interest) =
        expected_market.hlp_yield_growth_indexes(MarketAsset::Base, MarketAsset::Quote);
    expected_external
        .base_yield_account
        .accrue(0, base_swap, base_interest)
        .unwrap();
    expected_external
        .quote_yield_account
        .accrue(0, quote_swap, quote_interest)
        .unwrap();
    expected_external.hlp_vault_srlp_balance += expected_receipt.srlp_amount;
    expected_external.hlp_mint_supply += expected_receipt.hlp_amount;
    expected_external.holder_hlp_token_balance += expected_receipt.hlp_amount;
    validate_hlp_owned_state(&expected_external, &expected_market, market_key).unwrap();

    let mut counterpart = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Quote, owner, owner).unwrap();
    let execution = benchmark
        .execute_hlp_entry(&mut external, &mut counterpart, request)
        .unwrap();
    assert_eq!(execution.market.receipt.native, expected_receipt);
    assert_eq!(execution.market.cash.base.reserve_vault_credit, 100_000);
    assert_eq!(market_bytes(benchmark.market()), market_bytes(&expected_market));
    assert_eq!(external.checkpoint(), expected_external.checkpoint());
}

#[test]
fn failed_hlp_entry_rolls_back_market_external_state_and_clock() {
    let (mut benchmark, _) = initialized_keyed_market();
    let owner = Pubkey::new_unique();
    let mut external = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Base, owner, owner).unwrap();
    let mut counterpart = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Quote, owner, owner).unwrap();
    let market_before = market_bytes(benchmark.market());
    let external_before = external.checkpoint();
    let clock_before = benchmark.clock();

    assert!(benchmark
        .execute_hlp_entry(
            &mut external,
            &mut counterpart,
            BenchmarkHlpEntryRequest {
                funding: BenchmarkHlpFundingSettlement::default(),
                clock: BenchmarkClock {
                    slot: 50_000,
                    unix_timestamp: 50_000,
                },
                target_transfer: BenchmarkTokenTransferOutcome {
                    source_debit: 100_000,
                    destination_credit: 100_000,
                },
                min_hlp_amount: u64::MAX,
                global_reduce_only: false,
            },
        )
        .is_err());
    assert_eq!(market_bytes(benchmark.market()), market_before);
    assert_eq!(external.checkpoint(), external_before);
    assert_eq!(benchmark.clock(), clock_before);
}

#[test]
fn hlp_aware_swap_atomically_settles_both_srlp_vaults() {
    let (mut benchmark, _) = initialized_keyed_market();
    let owner = Pubkey::new_unique();
    let mut base_external =
        BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Base, owner, owner).unwrap();
    let mut quote_external =
        BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Quote, owner, owner).unwrap();
    benchmark
        .execute_hlp_entry(
            &mut base_external,
            &mut quote_external,
            BenchmarkHlpEntryRequest {
                funding: BenchmarkHlpFundingSettlement::default(),
                clock: BenchmarkClock {
                    slot: 2,
                    unix_timestamp: 2,
                },
                target_transfer: BenchmarkTokenTransferOutcome {
                    source_debit: 100_000,
                    destination_credit: 100_000,
                },
                min_hlp_amount: 1,
                global_reduce_only: false,
            },
        )
        .unwrap();
    benchmark
        .execute_hlp_entry(
            &mut quote_external,
            &mut base_external,
            BenchmarkHlpEntryRequest {
                funding: BenchmarkHlpFundingSettlement::default(),
                clock: BenchmarkClock {
                    slot: 3,
                    unix_timestamp: 3,
                },
                target_transfer: BenchmarkTokenTransferOutcome {
                    source_debit: 200_000,
                    destination_credit: 200_000,
                },
                min_hlp_amount: 1,
                global_reduce_only: false,
            },
        )
        .unwrap();
    let swap_request = BenchmarkSwapRequest {
        asset_in: MarketAsset::Base,
        reserve_credit: 100_000,
        protocol_fee_bps: 0,
        protocol_auction_split: ProtocolAuctionSplit::default(),
    };
    let preview = benchmark.preview_swap(swap_request).unwrap();
    assert!(
        preview.base_rebalance.srlp_mint_amount > 0
            || preview.base_rebalance.srlp_burn_amount > 0
            || preview.quote_rebalance.srlp_mint_amount > 0
            || preview.quote_rebalance.srlp_burn_amount > 0
    );
    let execution = benchmark
        .execute_swap_with_hlp(
            &mut base_external,
            &mut quote_external,
            BenchmarkHlpAwareSwapRequest {
                swap: swap_request,
                base_hlp_interest_transfer: BenchmarkTokenTransferOutcome {
                    source_debit: preview.base_rebalance.interest_paid,
                    destination_credit: preview.base_rebalance.interest_paid,
                },
                quote_hlp_interest_transfer: BenchmarkTokenTransferOutcome {
                    source_debit: preview.quote_rebalance.interest_paid,
                    destination_credit: preview.quote_rebalance.interest_paid,
                },
                protocol_interest_fee_bps: 2_000,
                protocol_auction_split: ProtocolAuctionSplit::default(),
            },
        )
        .unwrap();
    assert_eq!(execution.swap, preview);
    assert_eq!(
        base_external.hlp_vault_srlp_balance(),
        benchmark.market().base_hlp_vault.srlp_shares
    );
    assert_eq!(
        quote_external.hlp_vault_srlp_balance(),
        benchmark.market().quote_hlp_vault.srlp_shares
    );
}

#[test]
fn hlp_entry_settles_both_vaults_and_rolls_back_bad_funding_credit() {
    let (mut benchmark, _) = initialized_keyed_market();
    let owner = Pubkey::new_unique();
    let mut base = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Base, owner, owner).unwrap();
    let mut quote = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Quote, owner, owner).unwrap();
    for target_base in [true, false] {
        let (target, other) = if target_base {
            (&mut base, &mut quote)
        } else {
            (&mut quote, &mut base)
        };
        benchmark
            .execute_hlp_entry(
                target,
                other,
                BenchmarkHlpEntryRequest {
                    clock: BenchmarkClock {
                        slot: 2,
                        unix_timestamp: 2,
                    },
                    funding: Default::default(),
                    target_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: 100_000,
                        destination_credit: 100_000,
                    },
                    min_hlp_amount: 1,
                    global_reduce_only: false,
                },
            )
            .unwrap();
    }
    let clock = BenchmarkClock {
        slot: 1_000_002,
        unix_timestamp: 400_002,
    };
    let mut preview = clone_market(benchmark.market()).unwrap();
    prepare_hlp_entry_market(&mut preview, MarketAsset::Base, base.hlp_mint_supply, clock, false).unwrap();
    let native = preview.deposit_single_sided(MarketAsset::Base, 50_000, 1).unwrap();
    assert!(native.rebalances.iter().all(|r| r.interest_paid > 0));
    let funding = BenchmarkHlpFundingSettlement {
        transfers: native.rebalances.map(|r| BenchmarkTokenTransferOutcome {
            source_debit: r.interest_paid,
            destination_credit: r.interest_paid,
        }),
        ..Default::default()
    };
    let request = BenchmarkHlpEntryRequest {
        clock,
        funding,
        target_transfer: BenchmarkTokenTransferOutcome {
            source_debit: 50_000,
            destination_credit: 50_000,
        },
        min_hlp_amount: 1,
        global_reduce_only: false,
    };
    let before = (
        market_bytes(benchmark.market()),
        base.checkpoint(),
        quote.checkpoint(),
        benchmark.clock(),
    );
    let mut wrong = request;
    wrong.funding.transfers[1].destination_credit += 1;
    assert!(benchmark.execute_hlp_entry(&mut base, &mut quote, wrong).is_err());
    assert_eq!(
        (
            market_bytes(benchmark.market()),
            base.checkpoint(),
            quote.checkpoint(),
            benchmark.clock()
        ),
        before
    );
    let result = benchmark.execute_hlp_entry(&mut base, &mut quote, request).unwrap();
    assert_eq!(
        result.market.cash.base.interest_vault_credit,
        funding.transfers[1].destination_credit
    );
    assert_eq!(
        result.market.cash.quote.interest_vault_credit,
        funding.transfers[0].destination_credit
    );
    assert_eq!(
        base.hlp_vault_srlp_balance,
        benchmark.market().base_hlp_vault.srlp_shares
    );
    assert_eq!(
        quote.hlp_vault_srlp_balance,
        benchmark.market().quote_hlp_vault.srlp_shares
    );
}

#[test]
fn maximum_hlp_entry_and_pro_rata_withdrawal_exhaust_owned_state() {
    let (mut benchmark, _) = initialized_keyed_market();
    let owner = Pubkey::new_unique();
    let mut external = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Base, owner, owner).unwrap();
    let mut counterpart = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Quote, owner, owner).unwrap();
    let entry_clock = BenchmarkClock {
        slot: 2,
        unix_timestamp: 2,
    };
    let plan = benchmark
        .maximize_hlp_entry(&external, entry_clock, 100_000, false)
        .unwrap()
        .unwrap();
    assert_eq!(plan.target_reserve_credit, 100_000);
    let entry = benchmark
        .execute_maximum_hlp_entry(
            &mut external,
            &mut counterpart,
            plan,
            BenchmarkTokenTransferOutcome {
                source_debit: 100_321,
                destination_credit: plan.target_reserve_credit,
            },
            BenchmarkHlpFundingSettlement::default(),
            false,
        )
        .unwrap();
    assert_eq!(
        entry.external_after.hlp_vault_srlp_balance,
        entry.market.receipt.native.srlp_amount
    );
    let initial_hlp = external.holder_hlp_token_balance();
    let initial_srlp = external.hlp_vault_srlp_balance();
    let first_burn = (initial_hlp / 3).max(1);
    let first_clock = BenchmarkClock {
        slot: 1_000_002,
        unix_timestamp: 1_000_002,
    };
    let first_preview = benchmark
        .preview_hlp_withdrawal(&external, first_clock, first_burn)
        .unwrap();
    assert!(first_preview.interest_paid > 0);
    let first = benchmark
        .execute_hlp_withdraw(
            &mut external,
            BenchmarkHlpWithdrawRequest {
                clock: first_clock,
                hlp_amount: first_burn,
                target_transfer: BenchmarkTokenTransferOutcome {
                    source_debit: first_preview.target_amount_out,
                    destination_credit: first_preview.target_amount_out.saturating_sub(1),
                },
                interest_transfer: BenchmarkTokenTransferOutcome {
                    source_debit: first_preview.interest_paid,
                    destination_credit: first_preview.interest_paid,
                },
                min_target_recipient_credit: first_preview.target_amount_out.saturating_sub(1),
                protocol_interest_fee_bps: 2_000,
                protocol_auction_split: ProtocolAuctionSplit::default(),
            },
        )
        .unwrap();
    assert_eq!(first.market.receipt.native, first_preview);
    assert_eq!(external.holder_hlp_token_balance(), initial_hlp - first_burn);
    assert_eq!(
        external.hlp_vault_srlp_balance(),
        initial_srlp - first_preview.srlp_amount
    );
    assert_eq!(
        external.checkpoint().measured_quote_interest_vault_credits,
        first_preview.interest_paid
    );

    let final_burn = external.holder_hlp_token_balance();
    let final_clock = BenchmarkClock {
        slot: 1_000_003,
        unix_timestamp: 1_000_003,
    };
    let final_preview = benchmark
        .preview_hlp_withdrawal(&external, final_clock, final_burn)
        .unwrap();
    let final_exit = benchmark
        .execute_hlp_withdraw(
            &mut external,
            BenchmarkHlpWithdrawRequest {
                clock: final_clock,
                hlp_amount: final_burn,
                target_transfer: BenchmarkTokenTransferOutcome {
                    source_debit: final_preview.target_amount_out,
                    destination_credit: final_preview.target_amount_out,
                },
                interest_transfer: BenchmarkTokenTransferOutcome {
                    source_debit: final_preview.interest_paid,
                    destination_credit: final_preview.interest_paid,
                },
                min_target_recipient_credit: final_preview.target_amount_out,
                protocol_interest_fee_bps: 2_000,
                protocol_auction_split: ProtocolAuctionSplit::default(),
            },
        )
        .unwrap();
    assert_eq!(final_exit.market.receipt.native, final_preview);
    assert_eq!(external.holder_hlp_token_balance(), 0);
    assert_eq!(external.hlp_mint_supply(), 0);
    assert_eq!(external.hlp_vault_srlp_balance(), 0);
    assert_eq!(benchmark.market().base_hlp_vault.hlp_supply, 0);
    assert_eq!(benchmark.market().base_hlp_vault.srlp_shares, 0);
}

#[test]
fn prepared_leverage_open_and_full_unwind_match_native_transitions_exactly() {
    let (mut benchmark, market_key) = initialized_keyed_market();
    let owner = Pubkey::new_unique();
    let position_id = Pubkey::new_unique();
    let referrer = Pubkey::new_unique();
    let referral =
        BenchmarkReferralOwnedState::initialize(&benchmark, MarketAsset::Base, referrer, referrer, 2_500, true)
            .unwrap();
    let token_balances = BenchmarkLeverageTokenBalances {
        owner_base_balance: 100_000,
        owner_quote_balance: 100_000,
        base_reserve_vault_balance: required_reserve_custody(&benchmark.market().base_side).unwrap(),
        quote_reserve_vault_balance: required_reserve_custody(&benchmark.market().quote_side).unwrap(),
        base_interest_vault_balance: benchmark.market().base_side.fees.interest_vault_balance,
        quote_interest_vault_balance: benchmark.market().quote_side.fees.interest_vault_balance,
        base_leverage_collateral_vault_balance: 0,
        quote_leverage_collateral_vault_balance: 0,
    };
    let mut leverage = BenchmarkLeverageOwnedState::initialize_for_open(
        &benchmark,
        owner,
        owner,
        position_id,
        token_balances,
        Some(referral),
    )
    .unwrap();
    let mut base_hlp = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Base, owner, owner).unwrap();
    let mut quote_hlp = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Quote, owner, owner).unwrap();
    let policy = BenchmarkLeveragePolicy {
        protocol_swap_fee_bps: 0,
        protocol_interest_fee_bps: 2_000,
        protocol_auction_split: ProtocolAuctionSplit::default(),
        max_referral_interest_share_bps: 5_000,
        global_reduce_only: false,
        collateral_mint_is_leverage_eligible: true,
        launch_same_transaction_guard_satisfied: true,
    };
    let open_request = BenchmarkPrepareLeverageOpenRequest {
        clock: BenchmarkClock {
            slot: 2,
            unix_timestamp: 2,
        },
        debt_asset: MarketAsset::Base,
        margin_transfer: BenchmarkTokenTransferOutcome {
            source_debit: 1_000,
            destination_credit: 1_000,
        },
        multiplier_bps: 20_000,
        min_collateral_out: 1,
        limit_price_nad: 0,
        requested_referrer: Some(referrer),
        policy,
    };
    let prepared = benchmark
        .prepare_leverage_open(&leverage, &base_hlp, &quote_hlp, open_request)
        .unwrap();
    let open_quote = prepared.quote();
    let mut expected_market = clone_market(&prepared.common.prepared_market).unwrap();
    let mut expected_position = empty_leverage_position();
    let (_, position_bump) = leverage_position_pda(market_key, owner, owner, position_id).unwrap();
    let expected_open = expected_market
        .open_leverage(
            &mut expected_position,
            owner,
            market_key,
            owner,
            position_id,
            open_quote.referral_partner,
            open_quote.referral_interest_share_bps,
            MarketAsset::Base,
            1_000,
            20_000,
            open_quote.swap.amount_out,
            prepared.common.prepared_swap.clone(),
            full_leverage_swap_fee_credit(open_quote.swap).unwrap(),
            2,
            2,
            position_bump,
            0,
            ProtocolAuctionSplit::default(),
            LeverageCollateralFee::default(),
        )
        .unwrap();
    let open = benchmark
        .execute_prepared_leverage_open(
            &mut leverage,
            &mut base_hlp,
            &mut quote_hlp,
            prepared,
            BenchmarkExecuteLeverageOpenRequest {
                collateral_transfer: BenchmarkTokenTransferOutcome {
                    source_debit: open_quote.swap.amount_out,
                    destination_credit: open_quote.swap.amount_out,
                },
                base_hlp_interest_transfer: BenchmarkTokenTransferOutcome::default(),
                quote_hlp_interest_transfer: BenchmarkTokenTransferOutcome::default(),
            },
        )
        .unwrap();
    assert_eq!(open.market.receipt.native, expected_open);
    assert_eq!(market_bytes(benchmark.market()), market_bytes(&expected_market));
    assert_eq!(
        leverage.position().unwrap().try_to_vec().unwrap(),
        expected_position.try_to_vec().unwrap()
    );
    assert_eq!(open.market.receipt.metrics.debt_amount, expected_open.debt_amount);
    assert!(!open.market.receipt.metrics.liquidatable);
    assert_eq!(
        leverage.checkpoint().referral.unwrap().configured_interest_share_bps,
        2_500
    );

    let collateral_amount = leverage.position().unwrap().collateral_amount;
    let close_request = BenchmarkPrepareLeverageCloseRequest {
        clock: open_request.clock,
        collateral_transfer: BenchmarkTokenTransferOutcome {
            source_debit: collateral_amount,
            destination_credit: collateral_amount,
        },
        min_residual_out: 0,
        policy,
    };
    let prepared_close = benchmark
        .prepare_leverage_close(&leverage, &base_hlp, &quote_hlp, close_request)
        .unwrap();
    let close_quote = prepared_close.quote();
    let mut expected_close_market = clone_market(&prepared_close.common.prepared_market).unwrap();
    let mut expected_close_position = clone_leverage_position(leverage.position().unwrap()).unwrap();
    let expected_close = *expected_close_market
        .close_leverage(
            &mut expected_close_position,
            0,
            prepared_close.common.prepared_swap.clone(),
            full_leverage_swap_fee_credit(close_quote.swap).unwrap(),
            0,
            ProtocolAuctionSplit::default(),
            close_request.clock.slot,
            LeverageCollateralFee::default(),
        )
        .unwrap();
    assert_eq!(expected_close.interest_paid, 0);
    let close = benchmark
        .execute_prepared_leverage_close(
            &mut leverage,
            &mut base_hlp,
            &mut quote_hlp,
            prepared_close,
            BenchmarkExecuteLeverageCloseRequest {
                residual_transfer: BenchmarkTokenTransferOutcome {
                    source_debit: expected_close.residual,
                    destination_credit: expected_close.residual,
                },
                interest_transfer: BenchmarkTokenTransferOutcome::default(),
                base_hlp_interest_transfer: BenchmarkTokenTransferOutcome::default(),
                quote_hlp_interest_transfer: BenchmarkTokenTransferOutcome::default(),
            },
        )
        .unwrap();
    assert_eq!(close.market.receipt.native, expected_close);
    assert_eq!(market_bytes(benchmark.market()), market_bytes(&expected_close_market));
    assert!(leverage.position().is_none());
    assert!(!close.leverage_after.position.account_exists);
    assert_eq!(close.market.receipt.referral_interest.referral_amount, 0);
    assert_eq!(
        close.market.receipt.residual_transfer.destination_credit,
        expected_close.residual
    );
}

#[test]
fn failed_prepared_leverage_execution_rolls_back_every_owned_account() {
    let (mut benchmark, _) = initialized_keyed_market();
    let owner = Pubkey::new_unique();
    let token_balances = BenchmarkLeverageTokenBalances {
        owner_base_balance: 100_000,
        owner_quote_balance: 100_000,
        base_reserve_vault_balance: required_reserve_custody(&benchmark.market().base_side).unwrap(),
        quote_reserve_vault_balance: required_reserve_custody(&benchmark.market().quote_side).unwrap(),
        base_interest_vault_balance: benchmark.market().base_side.fees.interest_vault_balance,
        quote_interest_vault_balance: benchmark.market().quote_side.fees.interest_vault_balance,
        ..BenchmarkLeverageTokenBalances::default()
    };
    let mut leverage = BenchmarkLeverageOwnedState::initialize_for_open(
        &benchmark,
        owner,
        owner,
        Pubkey::new_unique(),
        token_balances,
        None,
    )
    .unwrap();
    let mut base_hlp = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Base, owner, owner).unwrap();
    let mut quote_hlp = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Quote, owner, owner).unwrap();
    let request = BenchmarkPrepareLeverageOpenRequest {
        clock: BenchmarkClock {
            slot: 2,
            unix_timestamp: 2,
        },
        debt_asset: MarketAsset::Base,
        margin_transfer: BenchmarkTokenTransferOutcome {
            source_debit: 1_000,
            destination_credit: 1_000,
        },
        multiplier_bps: 20_000,
        min_collateral_out: 1,
        limit_price_nad: 0,
        requested_referrer: None,
        policy: BenchmarkLeveragePolicy {
            protocol_swap_fee_bps: 0,
            protocol_interest_fee_bps: 0,
            protocol_auction_split: ProtocolAuctionSplit::default(),
            max_referral_interest_share_bps: BPS_DENOMINATOR,
            global_reduce_only: false,
            collateral_mint_is_leverage_eligible: true,
            launch_same_transaction_guard_satisfied: true,
        },
    };
    let prepared = benchmark
        .prepare_leverage_open(&leverage, &base_hlp, &quote_hlp, request)
        .unwrap();
    let quote = prepared.quote();
    let market_before = market_bytes(benchmark.market());
    let clock_before = benchmark.clock();
    let leverage_before = leverage.checkpoint();
    let base_before = base_hlp.checkpoint();
    let quote_before = quote_hlp.checkpoint();
    assert!(benchmark
        .execute_prepared_leverage_open(
            &mut leverage,
            &mut base_hlp,
            &mut quote_hlp,
            prepared,
            BenchmarkExecuteLeverageOpenRequest {
                collateral_transfer: BenchmarkTokenTransferOutcome {
                    source_debit: quote.swap.amount_out.saturating_sub(1),
                    destination_credit: quote.swap.amount_out.saturating_sub(1),
                },
                base_hlp_interest_transfer: BenchmarkTokenTransferOutcome::default(),
                quote_hlp_interest_transfer: BenchmarkTokenTransferOutcome::default(),
            },
        )
        .is_err());
    assert_eq!(market_bytes(benchmark.market()), market_before);
    assert_eq!(benchmark.clock(), clock_before);
    assert_eq!(leverage.checkpoint(), leverage_before);
    assert_eq!(base_hlp.checkpoint(), base_before);
    assert_eq!(quote_hlp.checkpoint(), quote_before);
}

#[test]
fn liquidation_bid_matches_native_transition_and_keeps_partial_collateral() {
    let (mut benchmark, mut position, _) = borrowed_liquidation_fixture(50_000);
    let clock = BenchmarkClock {
        slot: 4,
        unix_timestamp: 4,
    };
    benchmark
        .execute_start_liquidation_auction(
            &mut position,
            BenchmarkStartLiquidationAuctionRequest {
                clock,
                debt_asset: MarketAsset::Base,
            },
        )
        .unwrap();
    let opening_reference = position.position().auction_floor_price_nad;
    benchmark
        .execute_swap(BenchmarkSwapRequest {
            asset_in: MarketAsset::Quote,
            reserve_credit: 100_000,
            protocol_fee_bps: 0,
            protocol_auction_split: ProtocolAuctionSplit::default(),
        })
        .unwrap();
    let clock = BenchmarkClock {
        slot: 100,
        unix_timestamp: 44,
    };
    benchmark.advance_to(clock).unwrap();
    let current_reference = benchmark
        .market()
        .liquidation_reference_price_nad(position.position(), MarketAsset::Base)
        .unwrap();
    assert_ne!(current_reference, opening_reference);
    let plan_request = BenchmarkLiquidationPlanRequest {
        clock,
        debt_asset: MarketAsset::Base,
        phase: BenchmarkLiquidationPhase::Bid,
        max_repay_credit: 1_000,
        collateral_reserve_credit: 0,
        protocol_swap_fee_bps: 0,
        protocol_auction_split: ProtocolAuctionSplit::default(),
    };
    let preview_request = BenchmarkLiquidationPreviewRequest { plan: plan_request };
    let preview = benchmark.preview_liquidation(&position, preview_request).unwrap();
    assert_eq!(
        preview.plan.auction_price_nad,
        position
            .position()
            .liquidation_auction_bid_price_nad(clock.unix_timestamp, current_reference)
            .unwrap()
    );
    assert_eq!(preview.native.socialized_loss, 0);
    assert!(preview.native.collateral_seized < 50_000);

    let mut native_market = clone_market(benchmark.market()).unwrap();
    let mut native_position = clone_borrow_position(position.position()).unwrap();
    advance_market_to_slot(&mut native_market, clock.slot).unwrap();
    let native_plan = liquidation_plan(
        &mut native_market,
        &mut native_position,
        plan_request,
        Default::default(),
    )
    .unwrap();
    let native_receipt = native_market
        .settle_liquidation(
            &mut native_position,
            MarketAsset::Base,
            native_plan.repay_credit,
            0,
            native_plan.terms,
            LiquidationPricing::ReferencePrice {
                debt_per_collateral_price_nad: native_plan.auction_price_nad,
            },
        )
        .unwrap();
    if native_receipt.interest_paid > 0 {
        native_market
            .record_receivable_income_credit(
                MarketAsset::Base,
                false,
                native_receipt.interest_paid,
                0,
                ProtocolAuctionSplit::default(),
                0,
            )
            .unwrap();
    }
    native_market.finalize_amm_transition(clock.slot).unwrap();
    native_market.refresh_risk_at_slot(clock.slot).unwrap();

    let mut balances = benchmark
        .liquidation_token_balances_at_custody_floor(1_000, 0, 0, 300_000)
        .unwrap();
    let execution = benchmark
        .execute_liquidation(
            &mut position,
            &mut balances,
            None,
            BenchmarkLiquidationExecuteRequest {
                preview: preview_request,
                max_repay_source_debit: 1_000,
                debt_transfer: exact_transfer(preview.plan.repay_credit),
                interest_transfer: exact_transfer(preview.native.interest_paid),
                collateral_transfer: exact_transfer(preview.native.collateral_to_liquidator),
                collateral_swap_transfer: BenchmarkTokenTransferOutcome::default(),
                owner_residual_transfer: BenchmarkTokenTransferOutcome::default(),
                junior_fee_transfer: exact_transfer(preview.native.junior_fee_funded),
                min_collateral_recipient_credit: 0,
                protocol_interest_fee_bps: 0,
                protocol_auction_split: ProtocolAuctionSplit::default(),
                referral_interest_amount: 0,
            },
        )
        .unwrap();
    assert_eq!(execution.market.receipt.native, native_receipt);
    assert_eq!(market_bytes(benchmark.market()), market_bytes(&native_market));
    assert_eq!(position_bytes(position.position()), position_bytes(&native_position));
    assert!(execution.position_after.quote_collateral > 0);
}

#[test]
fn floor_shortfall_closes_automatically_and_late_failure_rolls_back() {
    let (mut benchmark, mut position, _) = borrowed_liquidation_fixture(1_000);
    let trigger = benchmark
        .execute_start_liquidation_auction(
            &mut position,
            BenchmarkStartLiquidationAuctionRequest {
                clock: BenchmarkClock {
                    slot: 4,
                    unix_timestamp: 4,
                },
                debt_asset: MarketAsset::Base,
            },
        )
        .unwrap();
    let floor_clock = BenchmarkClock {
        slot: 100,
        unix_timestamp: trigger.market.receipt.first_floor_unix_timestamp,
    };
    let plan_request = BenchmarkLiquidationPlanRequest {
        clock: floor_clock,
        debt_asset: MarketAsset::Base,
        phase: BenchmarkLiquidationPhase::Floor,
        max_repay_credit: 0,
        collateral_reserve_credit: 995,
        protocol_swap_fee_bps: 0,
        protocol_auction_split: ProtocolAuctionSplit::default(),
    };
    let preview_request = BenchmarkLiquidationPreviewRequest { plan: plan_request };
    let preview = benchmark.preview_liquidation(&position, preview_request).unwrap();
    assert_eq!(preview.plan.caller_bounty, 5);
    assert_eq!(preview.plan.collateral_swap_debit, 995);
    assert_eq!(preview.native.collateral_seized, 1_000);
    assert!(preview.native.socialized_loss > 0);
    let mut balances = benchmark
        .liquidation_token_balances_at_custody_floor(0, 0, 0, 300_000)
        .unwrap();
    let valid_request = BenchmarkLiquidationExecuteRequest {
        preview: preview_request,
        max_repay_source_debit: 0,
        debt_transfer: BenchmarkTokenTransferOutcome::default(),
        interest_transfer: exact_transfer(preview.native.interest_paid),
        collateral_transfer: exact_transfer(preview.plan.caller_bounty),
        collateral_swap_transfer: exact_transfer(preview.plan.collateral_swap_debit),
        owner_residual_transfer: exact_transfer(preview.owner_residual),
        junior_fee_transfer: BenchmarkTokenTransferOutcome::default(),
        min_collateral_recipient_credit: 0,
        protocol_interest_fee_bps: 0,
        protocol_auction_split: ProtocolAuctionSplit::default(),
        referral_interest_amount: 0,
    };
    let market_before = market_bytes(benchmark.market());
    let position_before = position_bytes(position.position());
    let balances_before = balances;
    assert!(benchmark
        .execute_liquidation(
            &mut position,
            &mut balances,
            None,
            BenchmarkLiquidationExecuteRequest {
                min_collateral_recipient_credit: preview.plan.caller_bounty.saturating_add(1),
                ..valid_request
            },
        )
        .is_err());
    assert_eq!(market_bytes(benchmark.market()), market_before);
    assert_eq!(position_bytes(position.position()), position_before);
    assert_eq!(balances, balances_before);

    let execution = benchmark
        .execute_liquidation(&mut position, &mut balances, None, valid_request)
        .unwrap();
    assert_eq!(
        execution.market.receipt.native.socialized_loss,
        preview.native.socialized_loss
    );
    assert_eq!(execution.market.receipt.native.remaining_debt, 0);
    assert_eq!(execution.position_after.quote_collateral, 0);
    assert_eq!(execution.position_after.auction_debt_asset, u8::MAX);
    let settled = benchmark.market();
    let final_price = settled.current_concentrated_spot_price_nad().unwrap().unwrap();
    assert_eq!(settled.amm.last_trade_price_nad, final_price);
    assert_eq!(settled.risk.cached_spot_base_price_nad, final_price);
    assert_eq!(settled.last_marginal_observation_nad, final_price);
}

#[test]
fn floor_closes_when_transfer_fees_leave_no_collateral_for_the_amm() {
    let (mut benchmark, mut position, _) = borrowed_liquidation_fixture(1);
    let trigger = benchmark
        .execute_start_liquidation_auction(
            &mut position,
            BenchmarkStartLiquidationAuctionRequest {
                clock: BenchmarkClock {
                    slot: 4,
                    unix_timestamp: 4,
                },
                debt_asset: MarketAsset::Base,
            },
        )
        .unwrap();
    let floor_clock = BenchmarkClock {
        slot: 100,
        unix_timestamp: trigger.market.receipt.first_floor_unix_timestamp,
    };
    let preview_request = BenchmarkLiquidationPreviewRequest {
        plan: BenchmarkLiquidationPlanRequest {
            clock: floor_clock,
            debt_asset: MarketAsset::Base,
            phase: BenchmarkLiquidationPhase::Floor,
            max_repay_credit: 0,
            collateral_reserve_credit: 0,
            protocol_swap_fee_bps: 0,
            protocol_auction_split: ProtocolAuctionSplit::default(),
        },
    };
    let preview = benchmark.preview_liquidation(&position, preview_request).unwrap();
    assert_eq!(preview.plan.collateral_consumed, 1);
    assert_eq!(preview.plan.caller_bounty, 0);
    assert_eq!(preview.plan.swap_output, 0);
    assert_eq!(preview.native.repaid_amount, 0);
    assert!(preview.native.socialized_loss > 0);

    let mut balances = benchmark
        .liquidation_token_balances_at_custody_floor(0, 0, 0, 300_000)
        .unwrap();
    let execution = benchmark
        .execute_liquidation(
            &mut position,
            &mut balances,
            None,
            BenchmarkLiquidationExecuteRequest {
                preview: preview_request,
                max_repay_source_debit: 0,
                debt_transfer: BenchmarkTokenTransferOutcome::default(),
                interest_transfer: BenchmarkTokenTransferOutcome::default(),
                collateral_transfer: BenchmarkTokenTransferOutcome::default(),
                collateral_swap_transfer: BenchmarkTokenTransferOutcome {
                    source_debit: 1,
                    destination_credit: 0,
                },
                owner_residual_transfer: BenchmarkTokenTransferOutcome::default(),
                junior_fee_transfer: BenchmarkTokenTransferOutcome::default(),
                min_collateral_recipient_credit: 0,
                protocol_interest_fee_bps: 0,
                protocol_auction_split: ProtocolAuctionSplit::default(),
                referral_interest_amount: 0,
            },
        )
        .unwrap();
    assert_eq!(execution.market.receipt.native.remaining_debt, 0);
    assert_eq!(execution.position_after.quote_collateral, 0);
    assert_eq!(execution.position_after.auction_debt_asset, u8::MAX);
}

#[test]
fn account_round_trip_preserves_every_market_atom() {
    let benchmark = initialized_market();
    let fork = benchmark.try_fork().unwrap();
    assert_eq!(market_bytes(benchmark.market()), market_bytes(fork.market()));
    assert_eq!(benchmark.clock(), fork.clock());
}

#[test]
fn public_interest_transition_witness_covers_zero_repay_and_writeoff() {
    let zero = BenchmarkPublicInterestTransition::checked(
        BenchmarkPublicInterestCheckpoint { base: 7, quote: 9 },
        BenchmarkPublicInterestCheckpoint { base: 7, quote: 9 },
        BenchmarkPublicInterestCheckpoint { base: 7, quote: 9 },
        BenchmarkPublicInterestPayments::default(),
    )
    .unwrap();
    assert_eq!(
        zero,
        BenchmarkPublicInterestTransition::identity(BenchmarkPublicInterestCheckpoint { base: 7, quote: 9 })
    );

    let repay = BenchmarkPublicInterestTransition::checked(
        BenchmarkPublicInterestCheckpoint { base: 100, quote: 0 },
        BenchmarkPublicInterestCheckpoint { base: 120, quote: 0 },
        BenchmarkPublicInterestCheckpoint { base: 70, quote: 0 },
        BenchmarkPublicInterestPayments {
            base: BenchmarkPublicInterestPayment {
                gross_cash_interest_paid: 50,
                net_interest_vault_credit: 49,
            },
            quote: BenchmarkPublicInterestPayment::default(),
        },
    )
    .unwrap();
    assert_eq!(repay.base.clock_accrued, 20);
    assert_eq!(repay.base.total_interest_removed, 50);
    assert_eq!(repay.base.interest_written_off, 0);
    assert_eq!(repay.base.net_interest_vault_credit, 49);

    let liquidation = BenchmarkPublicInterestTransition::checked(
        BenchmarkPublicInterestCheckpoint { base: 100, quote: 0 },
        BenchmarkPublicInterestCheckpoint { base: 120, quote: 0 },
        BenchmarkPublicInterestCheckpoint { base: 10, quote: 0 },
        BenchmarkPublicInterestPayments {
            base: BenchmarkPublicInterestPayment {
                gross_cash_interest_paid: 40,
                net_interest_vault_credit: 40,
            },
            quote: BenchmarkPublicInterestPayment::default(),
        },
    )
    .unwrap();
    assert_eq!(liquidation.base.total_interest_removed, 110);
    assert_eq!(liquidation.base.interest_written_off, 70);
    assert_eq!(
        liquidation.base.gross_cash_interest_paid + liquidation.base.interest_written_off,
        liquidation.base.total_interest_removed
    );
}

#[test]
fn public_interest_transition_exposes_share_rounding_and_fails_closed() {
    let rounded = BenchmarkPublicInterestTransition::checked(
        BenchmarkPublicInterestCheckpoint { base: 100, quote: 0 },
        BenchmarkPublicInterestCheckpoint { base: 100, quote: 0 },
        BenchmarkPublicInterestCheckpoint { base: 101, quote: 0 },
        BenchmarkPublicInterestPayments::default(),
    )
    .unwrap();
    assert_eq!(rounded.base.transition_interest_created, 1);
    assert_eq!(rounded.base.total_interest_removed, 0);

    assert!(BenchmarkPublicInterestTransition::checked(
        BenchmarkPublicInterestCheckpoint { base: 100, quote: 0 },
        BenchmarkPublicInterestCheckpoint { base: 100, quote: 0 },
        BenchmarkPublicInterestCheckpoint { base: 99, quote: 0 },
        BenchmarkPublicInterestPayments {
            base: BenchmarkPublicInterestPayment {
                gross_cash_interest_paid: 2,
                net_interest_vault_credit: 2,
            },
            quote: BenchmarkPublicInterestPayment::default(),
        },
    )
    .is_err());
}

#[test]
fn public_interest_checkpoint_clamps_rounding_principal_above_debt() {
    let mut benchmark = initialized_market();
    benchmark.market.debt.fixed_base_shares = 1;
    benchmark.market.debt.fixed_base_principal = 2;
    benchmark.market.debt.isolated_base_shares = 1;
    benchmark.market.debt.isolated_base_principal = 2;
    let checkpoint = benchmark.public_interest_checkpoint().unwrap();
    assert_eq!(checkpoint.base, 0);
    assert_eq!(checkpoint.quote, 0);
}
