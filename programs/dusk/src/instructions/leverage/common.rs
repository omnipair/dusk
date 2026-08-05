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
        let manager_fee_bps = market.config.manager_fee_bps;
        record_inline_hlp_interest_credit(
            market,
            borrowed_asset,
            interest_credit,
            manager_fee_bps,
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
    manager_fee_bps: u16,
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
        manager_fee_bps,
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
    use super::*;
    use anchor_lang::solana_program::program_option::COption;
    use spl_token_2022::{
        extension::{
            transfer_fee::{TransferFee, TransferFeeConfig},
            BaseStateWithExtensionsMut, ExtensionType, StateWithExtensionsMut,
        },
        state::Mint as SplToken2022Mint,
    };

    #[test]
    fn leverage_market_pda_runtime_guard_matches_canonical_market_seeds() {
        let mut market = Market::default();
        market.base_side.asset_mint = Pubkey::new_unique();
        market.quote_side.asset_mint = Pubkey::new_unique();
        market.params_hash = [7; 32];
        let (market_key, bump) = Pubkey::find_program_address(
            &[
                MARKET_V2_SEED_PREFIX,
                market.base_side.asset_mint.as_ref(),
                market.quote_side.asset_mint.as_ref(),
                market.params_hash.as_ref(),
            ],
            &crate::ID,
        );
        market.bump = bump;

        validate_leverage_market_pda(&market, market_key).unwrap();
        assert!(validate_leverage_market_pda(&market, Pubkey::new_unique()).is_err());
    }

    #[test]
    fn leverage_futarchy_pda_runtime_guard_matches_canonical_authority_seed() {
        let (futarchy_authority_key, bump) =
            Pubkey::find_program_address(&[FUTARCHY_AUTHORITY_SEED_PREFIX], &crate::ID);

        validate_leverage_futarchy_pda(bump, futarchy_authority_key).unwrap();
        assert!(validate_leverage_futarchy_pda(bump, Pubkey::new_unique()).is_err());
    }

    #[test]
    fn leverage_public_paths_validate_runtime_control_pdas_before_mutation() {
        let open = include_str!("open_leverage.rs");
        let close = include_str!("close_leverage.rs");
        let decrease = include_str!("decrease_leverage.rs");
        let liquidate = include_str!("liquidate_leverage.rs");
        let lib = include_str!("../../lib.rs");
        let boxed_runtime_guard = "validate_leverage_market_pda(&self.market, self.market.key())?;";
        let futarchy_guard =
            "validate_leverage_futarchy_pda(self.futarchy_authority.bump, self.futarchy_authority.key())?;";
        let boxed_account = "#[account(mut)]\n    pub market: Box<Account<'info, Market>>";

        for (source, validator, handler) in [
            (open, "pub fn validate_at", "pub fn handle_open"),
            (liquidate, "pub fn validate_at", "pub fn handle_liquidate"),
        ] {
            assert!(source.contains(boxed_account));
            let validator_start = source.find(validator).unwrap();
            let guard = source.find(boxed_runtime_guard).unwrap();
            let futarchy_guard = source.find(futarchy_guard).unwrap();
            let handler_start = source.find(handler).unwrap();
            assert!(validator_start < guard && guard < handler_start);
            assert!(validator_start < futarchy_guard && futarchy_guard < handler_start);
        }

        assert!(decrease.contains(boxed_account));
        let validator_start = decrease.find("pub fn validate_at").unwrap();
        let guard = decrease.find(boxed_runtime_guard).unwrap();
        let futarchy = decrease.find(futarchy_guard).unwrap();
        let handler_start = decrease.find("pub fn handle_decrease").unwrap();
        assert!(validator_start < guard && guard < handler_start);
        assert!(validator_start < futarchy && futarchy < handler_start);

        assert!(close.contains(boxed_account));
        assert_eq!(close.matches("self.validate_common").count(), 2);
        let common_start = close.find("fn validate_common").unwrap();
        let guard = close.find(boxed_runtime_guard).unwrap();
        let futarchy_guard = close.find(futarchy_guard).unwrap();
        let owner_validation = close.find("pub fn validate_at").unwrap();
        let delegated_validation = close.find("pub fn validate_delegated_at").unwrap();
        let handler_start = close.find("pub fn handle_close").unwrap();
        assert!(common_start < guard);
        assert!(common_start < futarchy_guard);
        assert!(guard < owner_validation && owner_validation < handler_start);
        assert!(futarchy_guard < owner_validation && owner_validation < handler_start);
        assert!(guard < delegated_validation && delegated_validation < handler_start);
        assert!(futarchy_guard < delegated_validation && delegated_validation < handler_start);

        for (entrypoint, validation, handler) in [
            (
                "pub fn open_leverage<'info>",
                "ctx.accounts.validate_at",
                "OpenLeverage::handle_open",
            ),
            (
                "pub fn close_leverage<'info>",
                "ctx.accounts.validate_at",
                "CloseLeverage::handle_close",
            ),
            (
                "pub fn delegated_close_leverage<'info>",
                "ctx.accounts.validate_delegated_at",
                "CloseLeverage::handle_delegated_close",
            ),
            (
                "pub fn decrease_leverage<'info>",
                "ctx.accounts.validate_at",
                "DecreaseLeverage::handle_decrease",
            ),
            (
                "pub fn liquidate_leverage<'info>",
                "ctx.accounts.validate_at",
                "LiquidateLeverage::handle_liquidate",
            ),
        ] {
            let entrypoint_start = lib.find(entrypoint).unwrap();
            let body = &lib[entrypoint_start..];
            let validation = body.find(validation).unwrap();
            let handler = body.find(handler).unwrap();
            assert!(validation < handler);
        }
    }

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

    #[test]
    fn token_2022_claimable_credit_is_split_proportionally() {
        let quote = LeverageSwapQuote {
            fee_credit: 100,
            fee_breakdown: crate::state::SwapFeeBreakdown {
                base_fee_debit: 60,
                distributed_surcharge_debit: 40,
                claimable_fee_debit: 100,
                ..crate::state::SwapFeeBreakdown::default()
            },
            ..LeverageSwapQuote::default()
        };

        let credit = LeverageSwapFeeCredit::from_total_actual_credit(&quote, 97).unwrap();

        assert_eq!(credit.base, 58);
        assert_eq!(credit.distributed_surcharge, 39);
    }

    #[test]
    fn retained_surcharge_never_enters_claimable_fee_credit() {
        let quote = LeverageSwapQuote {
            fee_credit: 30,
            fee_breakdown: crate::state::SwapFeeBreakdown {
                base_fee_debit: 30,
                dynamic_surcharge_debit: 70,
                retained_surcharge: 70,
                distributed_surcharge_debit: 0,
                claimable_fee_debit: 30,
                ..crate::state::SwapFeeBreakdown::default()
            },
            ..LeverageSwapQuote::default()
        };

        let credit = LeverageSwapFeeCredit::from_total_actual_credit(&quote, 29).unwrap();

        assert_eq!(credit.base, 29);
        assert_eq!(credit.distributed_surcharge, 0);
    }

    #[test]
    fn ten_percent_transfer_fee_collateral_is_rejected_before_health_admission() {
        let mint_len =
            ExtensionType::try_calculate_account_len::<SplToken2022Mint>(&[ExtensionType::TransferFeeConfig]).unwrap();
        let mut mint_data = vec![0_u8; mint_len];
        {
            let mut mint = StateWithExtensionsMut::<SplToken2022Mint>::unpack_uninitialized(&mut mint_data).unwrap();
            let fee = TransferFee {
                epoch: 0_u64.into(),
                maximum_fee: u64::MAX.into(),
                transfer_fee_basis_points: 1_000_u16.into(),
            };
            let config = mint.init_extension::<TransferFeeConfig>(true).unwrap();
            config.older_transfer_fee = fee;
            config.newer_transfer_fee = fee;
            mint.base = SplToken2022Mint {
                mint_authority: COption::None,
                supply: 0,
                decimals: 6,
                is_initialized: true,
                freeze_authority: COption::None,
            };
            mint.pack_base();
            mint.init_account_type().unwrap();
        }

        let mint_key = Pubkey::new_unique();
        let owner = spl_token_2022::ID;
        let mut lamports = 1;
        let mint_info = AccountInfo::new(&mint_key, false, false, &mut lamports, &mut mint_data, &owner, false, 0);
        let mint = InterfaceAccount::<Mint>::try_from(&mint_info).unwrap();

        // The old health path stored the first net credit (900), then quoted
        // all 900 as unwind input even though the second transfer credits 810.
        let configured_fee = TransferFee {
            epoch: 0_u64.into(),
            maximum_fee: u64::MAX.into(),
            transfer_fee_basis_points: 1_000_u16.into(),
        };
        let gross_swap_output = 1_000;
        let stored_collateral = configured_fee.calculate_post_fee_amount(gross_swap_output).unwrap();
        let actual_unwind_credit = configured_fee.calculate_post_fee_amount(stored_collateral).unwrap();
        assert_eq!(stored_collateral, 900);
        assert_eq!(actual_unwind_credit, 810);
        assert_eq!(
            validate_leverage_collateral_risk_mint(&mint).unwrap_err(),
            error!(ErrorCode::InvalidLeverageCollateralMint)
        );
    }

    #[test]
    fn token_2022_collateral_without_transfer_fee_extension_remains_supported() {
        let mint_len = ExtensionType::try_calculate_account_len::<SplToken2022Mint>(&[]).unwrap();
        let mut mint_data = vec![0_u8; mint_len];
        {
            let mut mint = StateWithExtensionsMut::<SplToken2022Mint>::unpack_uninitialized(&mut mint_data).unwrap();
            mint.base = SplToken2022Mint {
                mint_authority: COption::None,
                supply: 0,
                decimals: 6,
                is_initialized: true,
                freeze_authority: COption::None,
            };
            mint.pack_base();
            mint.init_account_type().unwrap();
        }

        let mint_key = Pubkey::new_unique();
        let owner = spl_token_2022::ID;
        let mut lamports = 1;
        let mint_info = AccountInfo::new(&mint_key, false, false, &mut lamports, &mut mint_data, &owner, false, 0);
        let mint = InterfaceAccount::<Mint>::try_from(&mint_info).unwrap();

        validate_leverage_collateral_risk_mint(&mint).unwrap();
    }
}
