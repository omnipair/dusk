/// Chooses indexed debt within one raw atom of the vault's proportional
/// opposite-asset srLP claim at the final reserve point.
fn canonical_debt_for_proportional_claim(
    non_debt_reserve: u128,
    hlp_srlp_shares: u64,
    total_srlp_supply: u64,
    borrow_index_nad: u128,
    quoted_debt: Option<u64>,
) -> Result<(u128, u64)> {
    canonical_debt_for_proportional_claim_with_carry(
        non_debt_reserve,
        hlp_srlp_shares,
        total_srlp_supply,
        borrow_index_nad,
        0,
        0,
        quoted_debt,
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HlpTrackingReference {
    pub(crate) principal_nav_nad: i128,
    pub(crate) loss_budget_nad: u128,
    /// Frozen public-borrow interest only; never an hLP-funding claim.
    pub(crate) base_unrealized_interest: u64,
    /// Frozen public-borrow interest only; never an hLP-funding claim.
    pub(crate) quote_unrealized_interest: u64,
    pub(crate) start_srlp_shares: u64,
    pub(crate) start_srlp_supply: u64,
}

pub(crate) fn hlp_end_to_end_tracking_delta(
    market: &Market,
    receipt: HlpRebalanceReceipt,
    final_prices: HlpCurvePrices,
) -> Result<i128> {
    let tracking = HlpTrackingReference {
        principal_nav_nad: receipt.tracking_start_nav_nad,
        loss_budget_nad: receipt.tracking_loss_budget_nad,
        base_unrealized_interest: receipt.tracking_base_unrealized_interest,
        quote_unrealized_interest: receipt.tracking_quote_unrealized_interest,
        start_srlp_shares: receipt.tracking_start_srlp_shares,
        start_srlp_supply: receipt.tracking_start_srlp_supply,
    };
    let values = current_hlp_inventory_values_nad_with_prices(market, receipt.target_asset, final_prices)?;
    let collateral = values
        .target_inventory_value_nad
        .checked_add(values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let final_principal_nav_nad = signed_value_difference(collateral, values.debt_value_nad)?;
    let (_, _, tracking_delta_nad) = hlp_tracking_deltas_nad(
        market,
        receipt.target_asset,
        final_prices,
        final_principal_nav_nad,
        tracking,
    )?;
    tracking_delta_nad
        .checked_sub(receipt.tracking_retained_contribution_nad)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn hlp_interest_claims_for_shares(
    base_unrealized_interest: u64,
    quote_unrealized_interest: u64,
    srlp_shares: u64,
    srlp_supply: u64,
) -> Result<(u64, u64)> {
    if srlp_shares == 0 {
        return Ok((0, 0));
    }
    require!(srlp_supply > 0, ErrorCode::BrokenInvariant);
    if base_unrealized_interest == 0 && quote_unrealized_interest == 0 {
        return Ok((0, 0));
    }
    let base_claim = u64::try_from(mul_div_u128(
        base_unrealized_interest as u128,
        srlp_shares as u128,
        srlp_supply as u128,
    )?)
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let quote_claim = u64::try_from(mul_div_u128(
        quote_unrealized_interest as u128,
        srlp_shares as u128,
        srlp_supply as u128,
    )?)
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    Ok((base_claim, quote_claim))
}

fn signed_asset_value_in_target_nad_with_prices(
    market: &Market,
    prices: HlpCurvePrices,
    asset: MarketAsset,
    amount: i128,
    target_asset: MarketAsset,
) -> Result<i128> {
    let magnitude = u64::try_from(amount.unsigned_abs()).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let value = i128::try_from(asset_value_in_target_nad_with_prices(
        market,
        prices,
        asset,
        magnitude,
        target_asset,
    )?)
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    if amount < 0 {
        value.checked_neg().ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    } else {
        Ok(value)
    }
}

fn hlp_frozen_interest_claim_delta_value_nad(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
    tracking: HlpTrackingReference,
) -> Result<i128> {
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    let final_srlp_supply = market.base_side.shares.srlp_supply;
    require_eq!(
        final_srlp_supply,
        market.quote_side.shares.srlp_supply,
        ErrorCode::BrokenInvariant
    );
    let (base_public_claim, quote_public_claim) = hlp_interest_claims_for_shares(
        tracking.base_unrealized_interest,
        tracking.quote_unrealized_interest,
        vault.srlp_shares,
        final_srlp_supply,
    )?;
    let (start_base_public_claim, start_quote_public_claim) = hlp_interest_claims_for_shares(
        tracking.base_unrealized_interest,
        tracking.quote_unrealized_interest,
        tracking.start_srlp_shares,
        tracking.start_srlp_supply,
    )?;
    let base_delta = i128::from(base_public_claim)
        .checked_sub(i128::from(start_base_public_claim))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let quote_delta = i128::from(quote_public_claim)
        .checked_sub(i128::from(start_quote_public_claim))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    signed_asset_value_in_target_nad_with_prices(market, prices, MarketAsset::Base, base_delta, target_asset)?
        .checked_add(signed_asset_value_in_target_nad_with_prices(
            market,
            prices,
            MarketAsset::Quote,
            quote_delta,
            target_asset,
        )?)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn hlp_tracking_deltas_nad(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
    final_principal_nav_nad: i128,
    tracking: HlpTrackingReference,
) -> Result<(i128, i128, i128)> {
    let principal_delta = final_principal_nav_nad
        .checked_sub(tracking.principal_nav_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let claim_delta = hlp_frozen_interest_claim_delta_value_nad(market, target_asset, prices, tracking)?;
    let combined_delta = principal_delta
        .checked_add(claim_delta)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok((principal_delta, claim_delta, combined_delta))
}

impl HlpTerminalWaterfallPlan {
    pub(crate) fn reference_recovery(self) -> u64 {
        self.reference_recovery_in_borrowed
    }
}
