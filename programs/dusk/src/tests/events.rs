use super::*;

#[test]
fn indexer_receipts_remain_compact() {
    let side = MarketSideSnapshot {
        spot_price_nad: 1,
        price_ema_nad: 2,
        swap_fee_growth_index_q64: 3,
        interest_growth_index_q64: 4,
    };
    let swap = SwapExecuted {
        market: Pubkey::new_unique(),
        trader: Pubkey::new_unique(),
        actor: Pubkey::new_unique(),
        position: None,
        origin: SwapOrigin::Spot,
        slot: 1,
        asset_in_side: 0,
        amount_in: 1,
        amount_out: 2,
        gross_amount_out: 2,
        fee_asset_side: 0,
        amount_in_after_fee: 3,
        base_fee: 4,
        divergence_fee: 5,
        volatility_fee: 6,
        retained_fee: 7,
        compounded_fee: 0,
        hlp_recovery_target_asset: 0,
        hlp_recovery_funding_gap: 0,
        hlp_recovery_matched_input: 0,
        hlp_recovery_bonus_output: 0,
        hlp_recovery_discount_bps: 0,
        hlp_recovery_critical: false,
        base_live_reserve: 8,
        quote_live_reserve: 9,
        ylp_supply: 10,
        base: side,
        quote: side,
    };
    let leverage_swap = LeverageSwapReceipt {
        asset_in_side: 0,
        fee_asset_side: 0,
        amount_in: 1,
        amount_out: 2,
        gross_amount_out: 2,
        amount_in_after_fee: 3,
        base_fee: 4,
        divergence_fee: 5,
        volatility_fee: 6,
        retained_fee: 7,
        compounded_fee: 0,
        claimable_fee_credit: 8,
        base_live_reserve: 9,
        quote_live_reserve: 10,
    };
    let liquidity_added = LiquidityAdded {
        market: Pubkey::new_unique(),
        owner: Pubkey::new_unique(),
        base_reserve_credit: 1,
        quote_reserve_credit: 2,
        ylp_amount: 3,
        ylp_supply: 4,
        base_live_reserve: 5,
        quote_live_reserve: 6,
        metadata: MarketEventMetadata::at_slot(Pubkey::new_unique(), Pubkey::new_unique(), 7),
    };
    let liquidity_removed = LiquidityRemoved {
        market: Pubkey::new_unique(),
        owner: Pubkey::new_unique(),
        ylp_amount: 1,
        base_reserve_debit: 2,
        quote_reserve_debit: 3,
        base_owner_credit: 4,
        quote_owner_credit: 5,
        ylp_supply: 6,
        base_live_reserve: 7,
        quote_live_reserve: 8,
        metadata: MarketEventMetadata::at_slot(Pubkey::new_unique(), Pubkey::new_unique(), 9),
    };
    let opened = HlpOpened {
        market: Pubkey::new_unique(),
        owner: Pubkey::new_unique(),
        asset_side: 0,
        deposit_amount: 1,
        borrowed_amount: 2,
        ylp_amount: 3,
        hlp_amount: 4,
        ylp_supply: 5,
        hlp_supply: 6,
        base_live_reserve: 7,
        quote_live_reserve: 8,
    };
    let closed = HlpClosed {
        market: Pubkey::new_unique(),
        owner: Pubkey::new_unique(),
        asset_side: 1,
        hlp_amount: 1,
        ylp_amount: 2,
        amount_out: 3,
        debt_repaid: 4,
        interest_paid: 5,
        ylp_supply: 6,
        hlp_supply: 7,
        base_live_reserve: 8,
        quote_live_reserve: 9,
    };
    let liquidated = BorrowPositionLiquidated {
        market: Pubkey::new_unique(),
        borrow_position: Pubkey::new_unique(),
        borrower: Pubkey::new_unique(),
        liquidator: Pubkey::new_unique(),
        debt_asset_side: 1,
        repaid_amount: 1,
        collateral_seized: 2,
        collateral_to_liquidator: 3,
        collateral_credit: 4,
        insurance_drawn: 5,
        socialized_loss: 6,
        remaining_debt: 7,
        base_collateral: 8,
        quote_collateral: 9,
        fixed_base_shares: 10,
        fixed_quote_shares: 11,
        global_health_base_contribution_for_quote_debt: 12,
        global_health_quote_contribution_for_base_debt: 13,
        base_liquidation_cf_bps: 14,
        quote_liquidation_cf_bps: 15,
        auction_debt_asset: u8::MAX,
        closed: false,
    };

    // 232 bytes of execution receipt plus the 104-byte post-swap market state.
    assert_eq!(side.try_to_vec().unwrap().len(), 48);
    assert_eq!(SwapExecuted::DISCRIMINATOR.len() + swap.try_to_vec().unwrap().len(), 336);
    assert_eq!(leverage_swap.try_to_vec().unwrap().len(), 98);
    assert_eq!(
        LiquidityAdded::DISCRIMINATOR.len() + liquidity_added.try_to_vec().unwrap().len(),
        192
    );
    assert_eq!(
        LiquidityRemoved::DISCRIMINATOR.len() + liquidity_removed.try_to_vec().unwrap().len(),
        208
    );
    assert_eq!(HlpOpened::DISCRIMINATOR.len() + opened.try_to_vec().unwrap().len(), 137);
    assert_eq!(HlpClosed::DISCRIMINATOR.len() + closed.try_to_vec().unwrap().len(), 145);
    assert_eq!(
        BorrowPositionLiquidated::DISCRIMINATOR.len() + liquidated.try_to_vec().unwrap().len(),
        271
    );
}

#[test]
fn liquidation_auction_receipts_stay_compact() {
    let started = LiquidationAuctionStarted {
        market: Pubkey::new_unique(),
        position: Pubkey::new_unique(),
        owner: Pubkey::new_unique(),
        auction_debt_asset: 1,
        auction_start_time: 2,
        auction_start_price_nad: 3,
        auction_floor_price_nad: 4,
    };
    let cancelled = LiquidationAuctionCancelled {
        market: Pubkey::new_unique(),
        position: Pubkey::new_unique(),
        owner: Pubkey::new_unique(),
        debt_asset_side: 1,
    };

    assert_eq!(
        LiquidationAuctionStarted::DISCRIMINATOR.len() + started.try_to_vec().unwrap().len(),
        129
    );
    assert_eq!(
        LiquidationAuctionCancelled::DISCRIMINATOR.len() + cancelled.try_to_vec().unwrap().len(),
        105
    );
}

#[test]
fn lp_transfer_receipt_stays_compact() {
    let transferred = LpTransferred {
        market: Pubkey::new_unique(),
        lp_mint: Pubkey::new_unique(),
        token_kind: 1,
        source_owner: Pubkey::new_unique(),
        destination_owner: Pubkey::new_unique(),
        amount: 1,
    };

    assert_eq!(
        LpTransferred::DISCRIMINATOR.len() + transferred.try_to_vec().unwrap().len(),
        145
    );
}
