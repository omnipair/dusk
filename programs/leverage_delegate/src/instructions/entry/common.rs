use super::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateLeverageEntryOrderArgs {
    pub order_id: u64,
    pub position_id: Pubkey,
    pub debt_asset: u8,
    /// Gross amount transferred into escrow. The order records the measured
    /// net credit so Token-2022 transfer fees cannot underfund execution.
    pub deposit_amount: u64,
    pub min_margin_amount: u64,
    pub executor_bounty: u64,
    pub multiplier_bps: u64,
    /// Conservative all-in Quote-per-Base execution limit.
    pub limit_price_nad: u64,
    pub min_collateral_out: u64,
    pub expiry_unix_timestamp: i64,
    pub referrer: Option<Pubkey>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct LeverageEntryOrderIdArgs {
    pub order_id: u64,
}

pub(super) fn verify_opened_position(
    position: &UncheckedAccount,
    order: &LeverageEntryOrder,
) -> Result<()> {
    require_keys_eq!(
        *position.owner,
        dusk::ID,
        LeverageDelegateError::InvalidOrder
    );
    let data = position.try_borrow_data()?;
    let mut data_slice: &[u8] = &data;
    let position = LeveragePosition::try_deserialize(&mut data_slice)
        .map_err(|_| LeverageDelegateError::InvalidOrder)?;
    require_keys_eq!(
        position.owner,
        order.owner,
        LeverageDelegateError::InvalidOrder
    );
    require_keys_eq!(
        position.market,
        order.market,
        LeverageDelegateError::InvalidOrder
    );
    require_keys_eq!(
        position.position_id,
        order.position_id,
        LeverageDelegateError::InvalidOrder
    );
    require!(
        position.debt_asset == order.debt_asset,
        LeverageDelegateError::InvalidOrder
    );
    Ok(())
}

pub(crate) fn escrow_margin_after_bounty(
    credited_amount: u64,
    executor_bounty: u64,
    minimum_margin: u64,
) -> Result<u64> {
    let margin = credited_amount
        .checked_sub(executor_bounty)
        .ok_or(LeverageDelegateError::InvalidOrder)?;
    require_gte!(margin, minimum_margin, LeverageDelegateError::InvalidOrder);
    require!(margin > 0, LeverageDelegateError::InvalidOrder);
    Ok(margin)
}
