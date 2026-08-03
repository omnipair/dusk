use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount};

use crate::{
    errors::ErrorCode,
    state::{Insurance, Market, MarketAsset},
};

/// Liquidation math records the gross collateral routed to insurance so that
/// `collateral_seized = collateral_to_liquidator + insurance_funded` remains
/// exact. A Token-2022 transfer fee makes less than that gross amount usable by
/// the insurance vault. Reconcile the already-recorded nominal addition to the
/// destination vault's measured net credit.
pub(super) fn reconcile_insurance_funding_credit(
    insurance: &mut Insurance,
    debt_asset: MarketAsset,
    gross_funding: u64,
    actual_credit: u64,
) -> Result<()> {
    require_gte!(gross_funding, actual_credit, ErrorCode::MarketMathOverflow);
    let transfer_fee = gross_funding
        .checked_sub(actual_credit)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let available = match debt_asset {
        MarketAsset::Base => &mut insurance.quote_available,
        MarketAsset::Quote => &mut insurance.base_available,
    };
    *available = available
        .checked_sub(transfer_fee)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(())
}

pub(super) fn validate_liquidation_accounts<'info>(
    market: &Account<'info, Market>,
    liquidator: Pubkey,
    debt_asset_mint: &InterfaceAccount<'info, Mint>,
    collateral_asset_mint: &InterfaceAccount<'info, Mint>,
    reserve_vault: &InterfaceAccount<'info, TokenAccount>,
    collateral_vault: &InterfaceAccount<'info, TokenAccount>,
    insurance_vault: &InterfaceAccount<'info, TokenAccount>,
    collateral_insurance_vault: &InterfaceAccount<'info, TokenAccount>,
    liquidator_debt_account: &InterfaceAccount<'info, TokenAccount>,
    liquidator_collateral_account: &InterfaceAccount<'info, TokenAccount>,
) -> Result<MarketAsset> {
    let debt_asset = market.asset_for_mint(debt_asset_mint.key())?;
    let (debt_side, collateral_side, insurance_vault_key, collateral_insurance_vault_key) = match debt_asset {
        MarketAsset::Base => (
            &market.base_side,
            &market.quote_side,
            market.insurance.base_vault,
            market.insurance.quote_vault,
        ),
        MarketAsset::Quote => (
            &market.quote_side,
            &market.base_side,
            market.insurance.quote_vault,
            market.insurance.base_vault,
        ),
    };
    require_keys_eq!(debt_side.asset_mint, debt_asset_mint.key(), ErrorCode::InvalidMint);
    require_keys_eq!(
        collateral_side.asset_mint,
        collateral_asset_mint.key(),
        ErrorCode::InvalidMint
    );
    require_keys_eq!(debt_side.reserve_vault, reserve_vault.key(), ErrorCode::InvalidVault);
    require_keys_eq!(
        collateral_side.collateral_vault,
        collateral_vault.key(),
        ErrorCode::InvalidVault
    );
    require_keys_eq!(insurance_vault_key, insurance_vault.key(), ErrorCode::InvalidVault);
    require_keys_eq!(
        collateral_insurance_vault_key,
        collateral_insurance_vault.key(),
        ErrorCode::InvalidVault
    );
    require_keys_eq!(reserve_vault.mint, debt_asset_mint.key(), ErrorCode::InvalidVault);
    require_keys_eq!(insurance_vault.mint, debt_asset_mint.key(), ErrorCode::InvalidVault);
    require_keys_eq!(
        collateral_insurance_vault.mint,
        collateral_asset_mint.key(),
        ErrorCode::InvalidVault
    );
    require_keys_eq!(
        collateral_vault.mint,
        collateral_asset_mint.key(),
        ErrorCode::InvalidVault
    );
    require_keys_eq!(reserve_vault.owner, market.key(), ErrorCode::InvalidVault);
    require_keys_eq!(insurance_vault.owner, market.key(), ErrorCode::InvalidVault);
    require_keys_eq!(collateral_insurance_vault.owner, market.key(), ErrorCode::InvalidVault);
    require_keys_eq!(collateral_vault.owner, market.key(), ErrorCode::InvalidVault);
    require_keys_eq!(
        liquidator_debt_account.mint,
        debt_asset_mint.key(),
        ErrorCode::InvalidTokenAccount
    );
    require_keys_eq!(
        liquidator_debt_account.owner,
        liquidator,
        ErrorCode::InvalidTokenAccount
    );
    require_keys_eq!(
        liquidator_collateral_account.mint,
        collateral_asset_mint.key(),
        ErrorCode::InvalidTokenAccount
    );
    require_keys_eq!(
        liquidator_collateral_account.owner,
        liquidator,
        ErrorCode::InvalidTokenAccount
    );
    Ok(debt_asset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spl_token_2022::extension::transfer_fee::TransferFee;

    fn active_transfer_fee() -> TransferFee {
        TransferFee {
            epoch: 0_u64.into(),
            maximum_fee: u64::MAX.into(),
            transfer_fee_basis_points: 300_u16.into(),
        }
    }

    #[test]
    fn token_2022_fee_reconciles_quote_insurance_to_actual_credit() {
        let gross_funding = 10_000;
        let actual_credit = active_transfer_fee().calculate_post_fee_amount(gross_funding).unwrap();
        let mut insurance = Insurance {
            // State after liquidation math added 10_000 gross to a prior 40_000.
            quote_available: 50_000,
            ..Insurance::default()
        };

        reconcile_insurance_funding_credit(&mut insurance, MarketAsset::Base, gross_funding, actual_credit).unwrap();

        assert_eq!(actual_credit, 9_700);
        assert_eq!(insurance.quote_available, 49_700);
    }

    #[test]
    fn token_2022_fee_reconciles_base_insurance_to_actual_credit() {
        let gross_funding = 10_000;
        let actual_credit = active_transfer_fee().calculate_post_fee_amount(gross_funding).unwrap();
        let mut insurance = Insurance {
            // State after liquidation math added 10_000 gross to a prior 40_000.
            base_available: 50_000,
            ..Insurance::default()
        };

        reconcile_insurance_funding_credit(&mut insurance, MarketAsset::Quote, gross_funding, actual_credit).unwrap();

        assert_eq!(actual_credit, 9_700);
        assert_eq!(insurance.base_available, 49_700);
    }

    #[test]
    fn insurance_credit_cannot_exceed_gross_funding() {
        let mut insurance = Insurance {
            quote_available: 500,
            ..Insurance::default()
        };

        assert!(reconcile_insurance_funding_credit(&mut insurance, MarketAsset::Base, 100, 101).is_err());
    }
}
