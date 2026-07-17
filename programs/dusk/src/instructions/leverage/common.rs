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
        require_supported_asset_mint, token_account_credit, token_program_for_mint, validate_fee_accounts,
        validate_interest_accounts, validate_owner_asset_account, validate_side_vault_accounts,
    },
    shared::token::{
        get_transfer_fee, transfer_from_vault_to_vault, transfer_from_vault_to_vault_with_remaining_accounts,
    },
    state::{Market, MarketAsset},
};

pub const LEVERAGE_DELEGATE_CLOSE: u32 = 1 << 0;
pub const LEVERAGE_DELEGATE_ADD_MARGIN: u32 = 1 << 1;
pub const LEVERAGE_DELEGATE_REMOVE_MARGIN: u32 = 1 << 2;
pub const LEVERAGE_DELEGATE_INCREASE: u32 = 1 << 3;
pub const LEVERAGE_DELEGATE_DECREASE: u32 = 1 << 4;
pub const LEVERAGE_DELEGATE_CLOSE_SETTLED: u32 = 1 << 5;
pub const LEVERAGE_DELEGATION_APPROVAL_MAGIC: [u8; 8] = *b"OMNILVDA";
pub const LEVERAGE_DELEGATION_APPROVAL_VERSION: u8 = 1;

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

pub fn approved_for(approved_actions: u32, action: u32) -> Result<()> {
    require!(
        approved_actions & action == action,
        ErrorCode::InvalidLeverageDelegation
    );
    Ok(())
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

    let account_metas = delegated_account_metas(accounts, protected_accounts, writable_protected_accounts)?;
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

fn delegated_account_metas(
    accounts: &[AccountInfo],
    protected_accounts: &[Pubkey],
    writable_protected_accounts: &[Pubkey],
) -> Result<Vec<AccountMeta>> {
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
    Ok(account_metas)
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
    additional_accounts: &[AccountInfo<'info>],
) -> Result<u64> {
    if total_fee == 0 {
        return Ok(0);
    }
    let fee_balance_before = fee_vault.amount;
    let asset_token_program = token_program_for_mint(asset_mint, token_program, token_2022_program)?;
    transfer_from_vault_to_vault_with_remaining_accounts(
        market.to_account_info(),
        reserve_vault.to_account_info(),
        fee_vault.to_account_info(),
        asset_mint.to_account_info(),
        asset_token_program,
        total_fee,
        asset_mint.decimals,
        &[&generate_market_seeds!(market)[..]],
        additional_accounts,
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
    market.side_mut(debt_asset).record_interest_credit(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approved_for_requires_action_bit() {
        assert!(approved_for(LEVERAGE_DELEGATE_CLOSE, LEVERAGE_DELEGATE_CLOSE).is_ok());
        assert!(approved_for(LEVERAGE_DELEGATE_CLOSE, LEVERAGE_DELEGATE_INCREASE).is_err());
    }

    #[test]
    fn delegation_approval_binds_close_context() {
        let program = Pubkey::new_unique();
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
            MarketAsset::Base,
            recipient,
            mint,
            123,
        );
        let mut data = Vec::new();
        approval.serialize(&mut data).unwrap();

        assert!(validate_delegation_approval(
            program,
            &data,
            program,
            LEVERAGE_DELEGATE_CLOSE,
            market,
            owner,
            position,
            delegation,
            MarketAsset::Base,
            recipient,
            mint,
            123,
        )
        .is_ok());
        assert!(validate_delegation_approval(
            program,
            &data,
            program,
            LEVERAGE_DELEGATE_CLOSE_SETTLED,
            market,
            owner,
            position,
            delegation,
            MarketAsset::Base,
            recipient,
            mint,
            123,
        )
        .is_err());
    }
}
