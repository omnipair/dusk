use anchor_lang::{
    prelude::*,
    solana_program::{
        instruction::{AccountMeta, Instruction},
        program::{get_return_data, invoke},
    },
};
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    generate_market_seeds,
    instructions::common::{
        require_supported_asset_mint, token_account_credit, token_account_info_amount, token_account_info_credit,
        token_program_for_mint, validate_interest_accounts, validate_owner_asset_account, validate_side_vault_accounts,
        HlpSwapAccountLayout, BASE_HLP_YLP_VAULT_INDEX, BASE_INTEREST_VAULT_INDEX, HLP_SWAP_ACCOUNT_PREFIX_LEN,
        HLP_YLP_MINT_INDEX, QUOTE_HLP_YLP_VAULT_INDEX, QUOTE_INTEREST_VAULT_INDEX,
    },
    instructions::liquidity::record_inline_hlp_interest_credit,
    instructions::referral::common::{accrue_referral_interest, ReferralInterestAccrualReceipt},
    shared::token::{
        get_transfer_fee_for_epoch, is_fee_free_mint, token_burn, token_mint_to,
        transfer_checked_with_remaining_accounts,
    },
    state::{
        FutarchyAuthority, HlpRebalanceReceipt, HlpYieldEligibility, LeverageSwapFeeCredit, LeverageSwapQuote, Market,
        MarketAsset, ReferralAccrual, ReferralPartner,
    },
};

pub const LEVERAGE_DELEGATE_CLOSE: u32 = 1 << 0;
pub const LEVERAGE_DELEGATE_ADD_MARGIN: u32 = 1 << 1;
pub const LEVERAGE_DELEGATE_REMOVE_MARGIN: u32 = 1 << 2;
pub const LEVERAGE_DELEGATE_INCREASE: u32 = 1 << 3;
pub const LEVERAGE_DELEGATE_DECREASE: u32 = 1 << 4;
pub const LEVERAGE_DELEGATE_CLOSE_SETTLED: u32 = 1 << 5;
pub const LEVERAGE_DELEGATION_APPROVAL_MAGIC: [u8; 8] = *b"OMNILVDA";
pub const LEVERAGE_DELEGATION_APPROVAL_VERSION: u8 = 1;

/// Validates the canonical Market PDA outside Anchor's generated account
/// parser. Keeping the seed-array construction out of `try_accounts` avoids
/// overlapping it with the large Market deserialization frame.
pub fn validate_leverage_market_pda(market: &Market, market_key: Pubkey) -> Result<()> {
    let expected = Pubkey::create_program_address(&generate_market_seeds!(market), &crate::ID)
        .map_err(|_| error!(ErrorCode::InvalidMarket))?;
    require_keys_eq!(market_key, expected, ErrorCode::InvalidMarket);
    Ok(())
}

/// Validates the global futarchy authority after account parsing so the large
/// Market account is not live beside Anchor's generated seed-check frame.
pub fn validate_leverage_futarchy_pda(futarchy_authority_bump: u8, futarchy_authority_key: Pubkey) -> Result<()> {
    let expected = Pubkey::create_program_address(
        &[FUTARCHY_AUTHORITY_SEED_PREFIX, &[futarchy_authority_bump]],
        &crate::ID,
    )
    .map_err(|_| error!(ErrorCode::InvalidFutarchyAuthority))?;
    require_keys_eq!(futarchy_authority_key, expected, ErrorCode::InvalidFutarchyAuthority);
    Ok(())
}

pub fn leverage_collateral_vault_pda(market: Pubkey, collateral_mint: Pubkey) -> Result<(Pubkey, u8)> {
    Pubkey::try_find_program_address(
        &[
            LEVERAGE_COLLATERAL_VAULT_SEED_PREFIX,
            market.as_ref(),
            collateral_mint.as_ref(),
        ],
        &crate::ID,
    )
    .ok_or_else(|| error!(ErrorCode::InvalidVault))
}

pub fn leverage_position_pda(market: Pubkey, position_id: Pubkey) -> Result<(Pubkey, u8)> {
    Pubkey::try_find_program_address(
        &[LEVERAGE_POSITION_SEED_PREFIX, market.as_ref(), position_id.as_ref()],
        &crate::ID,
    )
    .ok_or_else(|| error!(ErrorCode::InvalidLeveragePosition))
}

#[allow(clippy::too_many_arguments)]
pub fn settle_inline_leverage_hlp<'info>(
    market: &mut Account<'info, Market>,
    futarchy_authority: &Account<'info, FutarchyAuthority>,
    debt_asset: MarketAsset,
    debt_mint: &InterfaceAccount<'info, Mint>,
    collateral_mint: &InterfaceAccount<'info, Mint>,
    debt_reserve_vault: &InterfaceAccount<'info, TokenAccount>,
    collateral_reserve_vault: &InterfaceAccount<'info, TokenAccount>,
    token_program: &Program<'info, Token>,
    token_2022_program: &Program<'info, Token2022>,
    remaining_accounts: &[AccountInfo<'info>],
    layout: HlpSwapAccountLayout,
    base_receipt: HlpRebalanceReceipt,
    quote_receipt: HlpRebalanceReceipt,
    interest_eligibility: HlpYieldEligibility,
) -> Result<()> {
    for receipt in [base_receipt, quote_receipt] {
        if receipt.ylp_mint_amount == 0 && receipt.ylp_burn_amount == 0 && receipt.interest_paid == 0 {
            continue;
        }
        require_eq!(
            layout.prefix_len,
            HLP_SWAP_ACCOUNT_PREFIX_LEN,
            ErrorCode::NotEnoughAccounts
        );
        require!(
            receipt.ylp_mint_amount == 0 || receipt.ylp_burn_amount == 0,
            ErrorCode::BrokenInvariant
        );
        let ylp_vault_index = if receipt.target_asset == MarketAsset::Base {
            BASE_HLP_YLP_VAULT_INDEX
        } else {
            QUOTE_HLP_YLP_VAULT_INDEX
        };
        let market_seeds = generate_market_seeds!(market);
        let signer_seeds = [&market_seeds[..]];
        if receipt.ylp_mint_amount > 0 {
            token_mint_to(
                market.to_account_info(),
                token_2022_program.to_account_info(),
                remaining_accounts[HLP_YLP_MINT_INDEX].clone(),
                remaining_accounts[ylp_vault_index].clone(),
                receipt.ylp_mint_amount,
                &signer_seeds,
            )?;
        }
        if receipt.ylp_burn_amount > 0 {
            token_burn(
                market.to_account_info(),
                token_2022_program.to_account_info(),
                remaining_accounts[HLP_YLP_MINT_INDEX].clone(),
                remaining_accounts[ylp_vault_index].clone(),
                receipt.ylp_burn_amount,
                &signer_seeds,
            )?;
        }
        if receipt.interest_paid == 0 {
            continue;
        }

        let borrowed_asset = receipt.target_asset.opposite();
        let interest_vault_index = if borrowed_asset == MarketAsset::Base {
            BASE_INTEREST_VAULT_INDEX
        } else {
            QUOTE_INTEREST_VAULT_INDEX
        };
        let interest_vault = &remaining_accounts[interest_vault_index];
        let interest_vault_balance_before = token_account_info_amount(interest_vault)?;
        let (reserve_vault, mint, token_program_account, decimals) = if borrowed_asset == debt_asset {
            (
                debt_reserve_vault.to_account_info(),
                debt_mint.to_account_info(),
                token_program_for_mint(debt_mint, token_program, token_2022_program)?,
                debt_mint.decimals,
            )
        } else {
            (
                collateral_reserve_vault.to_account_info(),
                collateral_mint.to_account_info(),
                token_program_for_mint(collateral_mint, token_program, token_2022_program)?,
                collateral_mint.decimals,
            )
        };
        transfer_checked_with_remaining_accounts(
            market.to_account_info(),
            reserve_vault,
            interest_vault.clone(),
            mint,
            token_program_account,
            receipt.interest_paid,
            decimals,
            &signer_seeds,
            layout.hook_accounts(remaining_accounts),
        )?;
        let interest_credit = token_account_info_credit(interest_vault_balance_before, interest_vault)?;
        record_inline_hlp_interest_credit(
            market,
            borrowed_asset,
            interest_credit,
            futarchy_authority.revenue_share.interest_bps,
            futarchy_authority.protocol_auction_split,
            interest_eligibility,
        )?;
    }
    Ok(())
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Default)]
pub struct DelegatedCpiArgs {
    pub before_ix_data: Vec<u8>,
    pub after_ix_data: Vec<u8>,
    pub before_accounts_len: u16,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct LeverageDelegationApproval {
    pub magic: [u8; 8],
    pub version: u8,
    pub action: u32,
    pub market: Pubkey,
    pub owner: Pubkey,
    pub position: Pubkey,
    pub delegation: Pubkey,
    pub debt_asset: u8,
    pub recipient_token_account: Pubkey,
    pub output_mint: Pubkey,
    pub output_amount: u64,
}

impl LeverageDelegationApproval {
    pub fn new(
        action: u32,
        market: Pubkey,
        owner: Pubkey,
        position: Pubkey,
        delegation: Pubkey,
        debt_asset: MarketAsset,
        recipient_token_account: Pubkey,
        output_mint: Pubkey,
        output_amount: u64,
    ) -> Self {
        Self {
            magic: LEVERAGE_DELEGATION_APPROVAL_MAGIC,
            version: LEVERAGE_DELEGATION_APPROVAL_VERSION,
            action,
            market,
            owner,
            position,
            delegation,
            debt_asset: debt_asset.code(),
            recipient_token_account,
            output_mint,
            output_amount,
        }
    }
}

pub fn split_delegated_accounts<'a, 'info>(
    accounts: &'a [AccountInfo<'info>],
    before_accounts_len: u16,
) -> Result<(&'a [AccountInfo<'info>], &'a [AccountInfo<'info>])> {
    let before_accounts_len = before_accounts_len as usize;
    require!(
        before_accounts_len <= accounts.len(),
        ErrorCode::InvalidLeverageDelegation
    );
    Ok(accounts.split_at(before_accounts_len))
}

pub fn invoke_delegated_callback<'info>(
    delegated_program: &UncheckedAccount<'info>,
    data: Vec<u8>,
    accounts: &[AccountInfo<'info>],
    protected_accounts: &[Pubkey],
    writable_protected_accounts: &[Pubkey],
) -> Result<()> {
    require!(!data.is_empty(), ErrorCode::InvalidLeverageDelegation);
    require!(delegated_program.executable, ErrorCode::InvalidLeverageDelegation);

    for (index, account) in accounts.iter().enumerate() {
        for prior in accounts.iter().take(index) {
            require_keys_neq!(account.key(), prior.key(), ErrorCode::InvalidLeverageDelegation);
        }
    }
    let mut account_metas = Vec::with_capacity(accounts.len());
    for account in accounts {
        let is_protected = protected_accounts.contains(account.key);
        let is_writable_protected = writable_protected_accounts.contains(account.key);
        if is_protected && !is_writable_protected {
            account_metas.push(AccountMeta::new_readonly(account.key(), false));
            continue;
        }
        if is_protected {
            require!(!account.is_signer, ErrorCode::InvalidLeverageDelegation);
        }
        account_metas.push(AccountMeta {
            pubkey: account.key(),
            is_signer: account.is_signer,
            is_writable: account.is_writable,
        });
    }
    let mut account_infos = Vec::with_capacity(accounts.len() + 1);
    account_infos.push(delegated_program.to_account_info());
    account_infos.extend(accounts.iter().cloned());

    invoke(
        &Instruction {
            program_id: delegated_program.key(),
            accounts: account_metas,
            data,
        },
        &account_infos,
    )
    .map_err(Into::into)
}

#[allow(clippy::too_many_arguments)]
pub fn invoke_delegated_approval_callback<'info>(
    delegated_program: &UncheckedAccount<'info>,
    data: Vec<u8>,
    accounts: &[AccountInfo<'info>],
    protected_accounts: &[Pubkey],
    writable_protected_accounts: &[Pubkey],
    expected_action: u32,
    expected_market: Pubkey,
    expected_owner: Pubkey,
    expected_position: Pubkey,
    expected_delegation: Pubkey,
    expected_debt_asset: MarketAsset,
    expected_recipient_token_account: Pubkey,
    expected_output_mint: Pubkey,
    expected_output_amount: u64,
) -> Result<()> {
    invoke_delegated_callback(
        delegated_program,
        data,
        accounts,
        protected_accounts,
        writable_protected_accounts,
    )?;

    let (program_id, data) = get_return_data().ok_or(ErrorCode::InvalidLeverageDelegation)?;
    validate_delegation_approval(
        program_id,
        &data,
        delegated_program.key(),
        expected_action,
        expected_market,
        expected_owner,
        expected_position,
        expected_delegation,
        expected_debt_asset,
        expected_recipient_token_account,
        expected_output_mint,
        expected_output_amount,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn validate_delegation_approval(
    program_id: Pubkey,
    data: &[u8],
    expected_program: Pubkey,
    expected_action: u32,
    expected_market: Pubkey,
    expected_owner: Pubkey,
    expected_position: Pubkey,
    expected_delegation: Pubkey,
    expected_debt_asset: MarketAsset,
    expected_recipient_token_account: Pubkey,
    expected_output_mint: Pubkey,
    expected_output_amount: u64,
) -> Result<()> {
    require_keys_eq!(program_id, expected_program, ErrorCode::InvalidLeverageDelegation);
    let mut data_ref = data;
    let approval =
        LeverageDelegationApproval::deserialize(&mut data_ref).map_err(|_| ErrorCode::InvalidLeverageDelegation)?;
    require!(data_ref.is_empty(), ErrorCode::InvalidLeverageDelegation);
    require!(
        approval.magic == LEVERAGE_DELEGATION_APPROVAL_MAGIC,
        ErrorCode::InvalidLeverageDelegation
    );
    require!(
        approval.version == LEVERAGE_DELEGATION_APPROVAL_VERSION,
        ErrorCode::InvalidLeverageDelegation
    );
    require!(approval.action == expected_action, ErrorCode::InvalidLeverageDelegation);
    require_keys_eq!(approval.market, expected_market, ErrorCode::InvalidLeverageDelegation);
    require_keys_eq!(approval.owner, expected_owner, ErrorCode::InvalidLeverageDelegation);
    require_keys_eq!(
        approval.position,
        expected_position,
        ErrorCode::InvalidLeverageDelegation
    );
    require_keys_eq!(
        approval.delegation,
        expected_delegation,
        ErrorCode::InvalidLeverageDelegation
    );
    require!(
        approval.debt_asset == expected_debt_asset.code(),
        ErrorCode::InvalidLeverageDelegation
    );
    require_keys_eq!(
        approval.recipient_token_account,
        expected_recipient_token_account,
        ErrorCode::InvalidLeverageDelegation
    );
    require_keys_eq!(
        approval.output_mint,
        expected_output_mint,
        ErrorCode::InvalidLeverageDelegation
    );
    require!(
        approval.output_amount == expected_output_amount,
        ErrorCode::InvalidLeverageDelegation
    );
    Ok(())
}

pub fn validate_leverage_mints<'info>(
    market: &Account<'info, Market>,
    debt_asset: MarketAsset,
    debt_mint: &InterfaceAccount<'info, Mint>,
    collateral_mint: &InterfaceAccount<'info, Mint>,
) -> Result<()> {
    let debt_side = market.side(debt_asset);
    let collateral_side = market.side(debt_asset.opposite());
    require_keys_eq!(debt_mint.key(), debt_side.asset_mint, ErrorCode::InvalidMint);
    require_keys_eq!(
        collateral_mint.key(),
        collateral_side.asset_mint,
        ErrorCode::InvalidMint
    );
    require_supported_asset_mint(debt_mint)?;
    require_supported_asset_mint(collateral_mint)?;
    Ok(())
}

/// Leverage health is evaluated against the collateral that can be returned to
/// the AMM on unwind. A mint with `TransferFeeConfig` can charge another fee on
/// that future vault-to-vault transfer, and its authority can change the fee
/// after a position opens. Reject the extension itself on every risk-increasing
/// path; Token-2022 mints without it remain supported.
pub fn validate_leverage_collateral_risk_mint(mint: &InterfaceAccount<Mint>) -> Result<()> {
    require!(is_fee_free_mint(mint)?, ErrorCode::InvalidLeverageCollateralMint);
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
    validate_side_vault_accounts(market, debt_asset.opposite(), collateral_mint, collateral_reserve_vault)?;
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

pub fn leverage_collateral_credit(mint: &InterfaceAccount<Mint>, gross_amount: u64, epoch: u64) -> Result<u64> {
    let fee = get_transfer_fee_for_epoch(&mint.to_account_info(), gross_amount, epoch)?;
    gross_amount
        .checked_sub(fee)
        .ok_or(ErrorCode::MarketMathOverflow.into())
}

pub fn leverage_swap_fee_credit(quote: &LeverageSwapQuote) -> Result<LeverageSwapFeeCredit> {
    let claimable_fee_debit = quote.fee_breakdown.claimable_fee_debit;
    require_eq!(claimable_fee_debit, quote.fee_credit, ErrorCode::BrokenInvariant);
    LeverageSwapFeeCredit::from_total_actual_credit(quote, claimable_fee_debit)
}

pub fn record_leverage_interest<'info>(
    market: &mut Account<'info, Market>,
    debt_asset: MarketAsset,
    debt_mint: &InterfaceAccount<'info, Mint>,
    debt_reserve_vault: &mut InterfaceAccount<'info, TokenAccount>,
    interest_vault: &mut InterfaceAccount<'info, TokenAccount>,
    token_program: &Program<'info, Token>,
    token_2022_program: &Program<'info, Token2022>,
    futarchy_authority: &Account<'info, FutarchyAuthority>,
    expected_referral_partner: Pubkey,
    referral_interest_share_bps: u16,
    referral_partner: Option<&Account<'info, ReferralPartner>>,
    referral_accrual: Option<&mut Account<'info, ReferralAccrual>>,
    interest_paid: u64,
    remaining_accounts: &[AccountInfo<'info>],
) -> Result<ReferralInterestAccrualReceipt> {
    if interest_paid == 0 {
        return accrue_referral_interest(
            expected_referral_partner,
            referral_interest_share_bps,
            futarchy_authority,
            referral_partner,
            referral_accrual,
            market.key(),
            debt_mint,
            0,
            0,
            futarchy_authority.revenue_share.interest_bps,
        );
    }
    let interest_vault_balance_before = interest_vault.amount;
    let debt_token_program = token_program_for_mint(debt_mint, token_program, token_2022_program)?;
    transfer_checked_with_remaining_accounts(
        market.to_account_info(),
        debt_reserve_vault.to_account_info(),
        interest_vault.to_account_info(),
        debt_mint.to_account_info(),
        debt_token_program,
        interest_paid,
        debt_mint.decimals,
        &[&generate_market_seeds!(market)[..]],
        remaining_accounts,
    )?;
    interest_vault.reload()?;
    let interest_vault_credit = token_account_credit(interest_vault_balance_before, interest_vault)?;
    let referral_receipt = accrue_referral_interest(
        expected_referral_partner,
        referral_interest_share_bps,
        futarchy_authority,
        referral_partner,
        referral_accrual,
        market.key(),
        debt_mint,
        interest_paid,
        interest_vault_credit,
        futarchy_authority.revenue_share.interest_bps,
    )?;
    market.side_mut(debt_asset).record_interest_credit(
        interest_vault_credit,
        futarchy_authority.revenue_share.interest_bps,
        futarchy_authority.protocol_auction_split,
        referral_receipt.quote.referral_amount,
    )?;
    Ok(referral_receipt)
}

pub fn validate_owner_debt_account<'info>(
    owner: Pubkey,
    debt_mint: &InterfaceAccount<'info, Mint>,
    account: &InterfaceAccount<'info, TokenAccount>,
) -> Result<()> {
    validate_owner_asset_account(owner, debt_mint, account)
}

#[cfg(test)]
mod tests {
    include!("../../tests/instructions/leverage/common.rs");
}
