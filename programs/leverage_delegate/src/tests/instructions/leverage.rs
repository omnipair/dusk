use super::*;

fn leverage_order() -> LeverageOrder {
    LeverageOrder {
        owner: Pubkey::new_unique(),
        market: Pubkey::new_unique(),
        position: Pubkey::new_unique(),
        order_id: 1,
        kind: ORDER_KIND_TAKE_PROFIT,
        trigger_closeout_price_nad: NAD,
        close_bps: BPS_DENOMINATOR,
        staged_margin: 0,
        staged_collateral_amount: 0,
        staged_remaining_collateral_amount: 0,
        staged_remaining_debt_shares: 0,
        staged_remaining_debt_principal: 0,
        staged_custody_token_account: Pubkey::default(),
        staged_output_mint: Pubkey::default(),
        staged_output_amount: 0,
        bump: 255,
    }
}

fn stage_close_settlement_reference(
    order: &mut LeverageOrder,
    margin: u64,
    custody_token_account: Pubkey,
    output_mint: Pubkey,
    output_amount: u64,
) {
    order.staged_margin = margin;
    order.staged_custody_token_account = custody_token_account;
    order.staged_output_mint = output_mint;
    order.staged_output_amount = output_amount;
}

fn require_staged_settlement_reference(
    order: &LeverageOrder,
    custody_token_account: Pubkey,
    output_mint: Pubkey,
    output_amount: u64,
) -> Result<()> {
    require_keys_eq!(
        order.staged_custody_token_account,
        custody_token_account,
        LeverageDelegateError::InvalidTokenAccount
    );
    require_keys_eq!(
        order.staged_output_mint,
        output_mint,
        LeverageDelegateError::InvalidTokenAccount
    );
    require!(
        order.staged_output_amount == output_amount,
        LeverageDelegateError::InvalidTokenAccount
    );
    Ok(())
}

fn trigger_met_reference(kind: u8, closeout_price_nad: u64, trigger_closeout_price_nad: u64) -> Result<()> {
    match kind {
        ORDER_KIND_TAKE_PROFIT => require!(
            closeout_price_nad >= trigger_closeout_price_nad,
            LeverageDelegateError::TriggerNotMet
        ),
        ORDER_KIND_STOP_LOSS => require!(
            closeout_price_nad <= trigger_closeout_price_nad,
            LeverageDelegateError::TriggerNotMet
        ),
        _ => return err!(LeverageDelegateError::InvalidOrder),
    }
    Ok(())
}

#[test]
fn order_kind_validation_accepts_only_tp_or_sl() {
    assert!(validate_order_kind(ORDER_KIND_TAKE_PROFIT).is_ok());
    assert!(validate_order_kind(ORDER_KIND_STOP_LOSS).is_ok());
    assert!(validate_order_kind(0).is_err());
}

#[test]
fn executor_incentive_is_five_percent_of_margin_capped_by_residual() {
    let incentive = |amount: u64, staged_margin: u64| {
        min(
            amount,
            ceil_div(
                staged_margin as u128 * EXECUTOR_INCENTIVE_BPS as u128,
                BPS_DENOMINATOR as u128,
            )
            .unwrap() as u64,
        )
    };
    assert_eq!(incentive(1_000, 10_000), 500);
    assert_eq!(incentive(300, 10_000), 300);
    assert_eq!(incentive(10, 1), 1);
}

#[test]
fn partial_order_incentive_is_bounded_by_realized_slice_equity() {
    let realized_slice_equity = 101;
    let incentive = ceil_div(
        realized_slice_equity as u128 * EXECUTOR_INCENTIVE_BPS as u128,
        BPS_DENOMINATOR as u128,
    )
    .unwrap() as u64;

    assert_eq!(incentive, 6);
    assert!(incentive <= realized_slice_equity);
    assert_eq!(realized_slice_equity - incentive, 95);
}

#[test]
fn reset_staged_settlement_clears_every_binding() {
    let mut order = leverage_order();
    order.staged_margin = 10_000;
    order.staged_collateral_amount = 50;
    order.staged_remaining_collateral_amount = 50;
    order.staged_remaining_debt_shares = 25;
    order.staged_remaining_debt_principal = 20;
    order.staged_custody_token_account = Pubkey::new_unique();
    order.staged_output_mint = Pubkey::new_unique();
    order.staged_output_amount = 123;
    reset_staged_settlement(&mut order);

    assert_eq!(order.staged_margin, 0);
    assert_eq!(order.staged_collateral_amount, 0);
    assert_eq!(order.staged_remaining_collateral_amount, 0);
    assert_eq!(order.staged_remaining_debt_shares, 0);
    assert_eq!(order.staged_remaining_debt_principal, 0);
    assert_eq!(order.staged_custody_token_account, Pubkey::default());
    assert_eq!(order.staged_output_mint, Pubkey::default());
    assert_eq!(order.staged_output_amount, 0);
}

#[test]
fn staged_settlement_defaults_reject_direct_after_close_cleanup() {
    let order = leverage_order();
    assert!(require_staged_settlement_reference(&order, Pubkey::new_unique(), Pubkey::new_unique(), 0).is_err());
}

#[test]
fn stage_close_settlement_binds_custody_mint_and_amount() {
    let mut order = leverage_order();
    let custody = Pubkey::new_unique();
    let mint = Pubkey::new_unique();
    stage_close_settlement_reference(&mut order, 10_000, custody, mint, 123);

    assert_eq!(order.staged_margin, 10_000);
    assert!(require_staged_settlement_reference(&order, custody, mint, 123).is_ok());
}

#[test]
fn staged_settlement_rejects_wrong_custody_mint_or_amount() {
    let mut order = leverage_order();
    let custody = Pubkey::new_unique();
    let mint = Pubkey::new_unique();
    stage_close_settlement_reference(&mut order, 10_000, custody, mint, 123);

    assert!(require_staged_settlement_reference(&order, Pubkey::new_unique(), mint, 123).is_err());
    assert!(require_staged_settlement_reference(&order, custody, Pubkey::new_unique(), 123).is_err());
    assert!(require_staged_settlement_reference(&order, custody, mint, 122).is_err());
}

#[test]
fn trigger_rules_match_take_profit_and_stop_loss_direction() {
    assert!(trigger_met_reference(ORDER_KIND_TAKE_PROFIT, 101, 100).is_ok());
    assert!(trigger_met_reference(ORDER_KIND_TAKE_PROFIT, 99, 100).is_err());
    assert!(trigger_met_reference(ORDER_KIND_STOP_LOSS, 99, 100).is_ok());
    assert!(trigger_met_reference(ORDER_KIND_STOP_LOSS, 101, 100).is_err());
    assert!(trigger_met_reference(0, 100, 100).is_err());
}


#[test]
fn delegated_close_uses_canonical_aggregate_cash_repayment() {
    use dusk::state::{Debt, MarketAsset};

    let index = (NAD as u128) * 103 / 100;
    let mut debt = Debt {
        base_borrow_index_nad: index,
        ..Debt::default()
    };
    let position_shares = debt.add_isolated_debt(MarketAsset::Base, 34).unwrap();
    debt.add_isolated_debt(MarketAsset::Base, 67).unwrap();

    let displayed_debt = Debt::shares_to_debt(position_shares, index).unwrap() as u64;
    let cash_repaid = debt
        .isolated_repayment_for_max(MarketAsset::Base, position_shares, u64::MAX)
        .unwrap()
        .cash_repaid;

    // Aggregate floor phases can make a full close cost one atom more than the
    // position's displayed debt. The callback must bind the amount Dusk will
    // actually accept, or its residual approval is one atom too high.
    assert_eq!(displayed_debt, 35);
    assert_eq!(cash_repaid, 36);
    assert_eq!(100_u64.checked_sub(displayed_debt), Some(65));
    assert_eq!(100_u64.checked_sub(cash_repaid), Some(64));
}

#[test]
fn approval_payload_binds_close_action_and_delegation() {
    let market = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let position = Pubkey::new_unique();
    let delegation = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let mint = Pubkey::new_unique();
    let approval = LeverageDelegationApproval::new(
        LEVERAGE_DELEGATE_CLOSE,
        market,
        owner,
        position,
        delegation,
        dusk::state::MarketAsset::Base,
        recipient,
        mint,
        456,
        123,
    );
    let mut data = Vec::new();
    approval.serialize(&mut data).unwrap();
    let decoded = LeverageDelegationApproval::deserialize(&mut data.as_slice()).unwrap();

    assert_eq!(decoded.action, LEVERAGE_DELEGATE_CLOSE);
    assert_eq!(decoded.market, market);
    assert_eq!(decoded.owner, owner);
    assert_eq!(decoded.position, position);
    assert_eq!(decoded.delegation, delegation);
    assert_eq!(decoded.debt_asset, dusk::state::MarketAsset::Base.code());
    assert_eq!(decoded.recipient_token_account, recipient);
    assert_eq!(decoded.output_mint, mint);
    assert_eq!(decoded.collateral_amount, 456);
    assert_eq!(decoded.output_amount, 123);
}
