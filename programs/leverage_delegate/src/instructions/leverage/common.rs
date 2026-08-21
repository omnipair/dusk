use super::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateLeverageOrderArgs {
    pub order_id: u64,
    pub kind: u8,
    pub trigger_closeout_price_nad: u64,
    /// Portion of the current position closed when triggered. `10_000` is a
    /// full close; smaller values realize one proportional slice.
    pub close_bps: u16,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateLeverageOrderArgs {
    pub order_id: u64,
    pub kind: u8,
    pub trigger_closeout_price_nad: u64,
    pub close_bps: u16,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CancelLeverageOrderArgs {
    pub order_id: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct ExecuteOrderArgs {
    pub order_id: u64,
}

pub(super) fn reset_staged_settlement(order: &mut LeverageOrder) {
    order.staged_margin = 0;
    order.staged_collateral_amount = 0;
    order.staged_remaining_collateral_amount = 0;
    order.staged_remaining_debt_shares = 0;
    order.staged_remaining_debt_principal = 0;
    order.staged_custody_token_account = Pubkey::default();
    order.staged_output_mint = Pubkey::default();
    order.staged_output_amount = 0;
}

pub(super) fn validate_order_kind(kind: u8) -> Result<()> {
    require!(
        kind == ORDER_KIND_TAKE_PROFIT || kind == ORDER_KIND_STOP_LOSS,
        LeverageDelegateError::InvalidOrder
    );
    Ok(())
}
