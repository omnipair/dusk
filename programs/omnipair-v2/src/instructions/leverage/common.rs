use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    generate_market_seeds,
    instructions::common::{
        require_supported_asset_mint, token_account_credit, token_program_for_mint,
        validate_fee_accounts, validate_interest_accounts, validate_owner_asset_account,
        validate_side_vault_accounts,
    },
    shared::token::{get_transfer_fee, transfer_from_vault_to_vault},
    state::{Market, MarketAsset},
};

pub fn validate_leverage_mints<'info>(
    market: &Account<'info, Market>,
    debt_asset: MarketAsset,
    debt_mint: &InterfaceAccount<'info, Mint>,
    collateral_mint: &InterfaceAccount<'info, Mint>,
) -> Result<()> {
    let debt_side = market.side(debt_asset)?;
    let collateral_side = market.side(debt_asset.opposite())?;
    require_keys_eq!(
        debt_mint.key(),
        debt_side.asset_mint,
        ErrorCode::InvalidMint
    );
    require_keys_eq!(
        collateral_mint.key(),
        collateral_side.asset_mint,
        ErrorCode::InvalidMint
    );
    require_supported_asset_mint(debt_mint)?;
    require_supported_asset_mint(collateral_mint)?;
    Ok(())
}

pub fn validate_leverage_reserve_accounts<'info>(
    market: &Account<'info, Market>,
    debt_asset: MarketAsset,
    debt_mint: &InterfaceAccount<'info, Mint>,
    collateral_mint: &InterfaceAccount<'info, Mint>,
    debt_reserve_vault: &InterfaceAccount<'info, TokenAccount>,
    collateral_reserve_vault: &InterfaceAccount<'info, TokenAccount>,
) -> Result<()> {
    validate_side_vault_accounts(market, debt_asset, debt_mint, debt_reserve_vault)?;
    validate_side_vault_accounts(
        market,
        debt_asset.opposite(),
        collateral_mint,
        collateral_reserve_vault,
    )?;
    Ok(())
}

pub fn validate_leverage_fee_account<'info>(
    market: &Account<'info, Market>,
    asset_mint: &InterfaceAccount<'info, Mint>,
    fee_vault: &InterfaceAccount<'info, TokenAccount>,
    expected_asset: MarketAsset,
) -> Result<()> {
    let fee_asset = validate_fee_accounts(market, asset_mint, fee_vault)?;
    require!(fee_asset == expected_asset, ErrorCode::InvalidVault);
    Ok(())
}

pub fn validate_leverage_interest_account<'info>(
    market: &Account<'info, Market>,
    debt_mint: &InterfaceAccount<'info, Mint>,
    interest_vault: &InterfaceAccount<'info, TokenAccount>,
    debt_asset: MarketAsset,
) -> Result<()> {
    let interest_asset = validate_interest_accounts(market, debt_mint, interest_vault)?;
    require!(interest_asset == debt_asset, ErrorCode::InvalidVault);
    Ok(())
}

pub fn leverage_collateral_credit(mint: &InterfaceAccount<Mint>, gross_amount: u64) -> Result<u64> {
    let fee = get_transfer_fee(&mint.to_account_info(), gross_amount)?;
    gross_amount
        .checked_sub(fee)
        .ok_or(ErrorCode::MarketMathOverflow.into())
}

pub fn move_leverage_swap_fee<'info>(
    market: &Account<'info, Market>,
    asset_mint: &InterfaceAccount<'info, Mint>,
    reserve_vault: &mut InterfaceAccount<'info, TokenAccount>,
    fee_vault: &mut InterfaceAccount<'info, TokenAccount>,
    token_program: &Program<'info, Token>,
    token_2022_program: &Program<'info, Token2022>,
    total_fee: u64,
) -> Result<u64> {
    if total_fee == 0 {
        return Ok(0);
    }
    let fee_balance_before = fee_vault.amount;
    let asset_token_program =
        token_program_for_mint(asset_mint, token_program, token_2022_program)?;
    transfer_from_vault_to_vault(
        market.to_account_info(),
        reserve_vault.to_account_info(),
        fee_vault.to_account_info(),
        asset_mint.to_account_info(),
        asset_token_program,
        total_fee,
        asset_mint.decimals,
        &[&generate_market_seeds!(market)[..]],
    )?;
    reserve_vault.reload()?;
    fee_vault.reload()?;
    token_account_credit(fee_balance_before, fee_vault)
}

pub fn record_leverage_interest<'info>(
    market: &mut Account<'info, Market>,
    debt_asset: MarketAsset,
    debt_mint: &InterfaceAccount<'info, Mint>,
    debt_reserve_vault: &mut InterfaceAccount<'info, TokenAccount>,
    interest_vault: &mut InterfaceAccount<'info, TokenAccount>,
    token_program: &Program<'info, Token>,
    token_2022_program: &Program<'info, Token2022>,
    manager_fee_bps: u16,
    protocol_fee_bps: u16,
    protocol_auction_split: crate::state::ProtocolAuctionSplit,
    interest_paid: u64,
) -> Result<()> {
    if interest_paid == 0 {
        return Ok(());
    }
    let debt_token_program = token_program_for_mint(debt_mint, token_program, token_2022_program)?;
    transfer_from_vault_to_vault(
        market.to_account_info(),
        debt_reserve_vault.to_account_info(),
        interest_vault.to_account_info(),
        debt_mint.to_account_info(),
        debt_token_program,
        interest_paid,
        debt_mint.decimals,
        &[&generate_market_seeds!(market)[..]],
    )?;
    debt_reserve_vault.reload()?;
    interest_vault.reload()?;
    market.side_mut(debt_asset)?.record_interest_credit(
        interest_paid,
        manager_fee_bps,
        protocol_fee_bps,
        protocol_auction_split,
    )?;
    Ok(())
}

pub fn validate_owner_debt_account<'info>(
    owner: Pubkey,
    debt_mint: &InterfaceAccount<'info, Mint>,
    account: &InterfaceAccount<'info, TokenAccount>,
) -> Result<()> {
    validate_owner_asset_account(owner, debt_mint, account)
}
