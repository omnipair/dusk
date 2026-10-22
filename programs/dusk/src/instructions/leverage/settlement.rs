use crate::transitions::amm::SwapRequest;
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
use spl_token_2022::extension::{
    transfer_fee::TransferFeeConfig, BaseStateWithExtensions, ExtensionType, StateWithExtensions,
};

use crate::transitions::liquidity::SwapCashPolicy;
use crate::{
    constants::*,
    errors::ErrorCode,
    generate_market_seeds,
    instructions::accounts::{
        require_reserve_custody, require_supported_asset_mint, token_account_credit, token_account_info_amount,
        token_account_info_credit, token_program_for_mint, validate_interest_accounts, validate_owner_asset_account,
        validate_side_vault_accounts, HlpSwapAccountLayout, BASE_HLP_YLP_VAULT_INDEX, BASE_INTEREST_VAULT_INDEX,
        HLP_SWAP_ACCOUNT_PREFIX_LEN, HLP_YLP_MINT_INDEX, QUOTE_HLP_YLP_VAULT_INDEX, QUOTE_INTEREST_VAULT_INDEX,
    },
    instructions::liquidity::record_inline_hlp_interest_credit,
    instructions::referral::accounting::ReferralInterestAccrualReceipt,
    state::{FutarchyAuthority, Market, MarketAsset, ReferralAccrual, ReferralPartner},
    token::{get_transfer_fee_for_epoch, token_burn, token_mint_to, transfer_checked_with_remaining_accounts},
    transitions::{
        HlpRebalanceReceipt, HlpYieldEligibility, LeverageCollateralFee, LeverageSwapFeeCredit, LeverageSwapQuote,
        PreparedLeverageSwap,
    },
};

pub const LEVERAGE_DELEGATE_CLOSE: u32 = 1 << 0;
pub const LEVERAGE_DELEGATE_ADD_MARGIN: u32 = 1 << 1;
pub const LEVERAGE_DELEGATE_INCREASE: u32 = 1 << 3;
pub const LEVERAGE_DELEGATE_DECREASE: u32 = 1 << 4;
pub const LEVERAGE_DELEGATE_CLOSE_SETTLED: u32 = 1 << 5;
/// Number of fixed hLP settlement accounts preceding token-hook accounts in
/// every leverage swap CPI. Periphery programs use this only to forward the
/// same hook suffix for their own custody transfers.
pub const LEVERAGE_HLP_ACCOUNT_PREFIX_LEN: usize = HLP_SWAP_ACCOUNT_PREFIX_LEN;
pub const LEVERAGE_DELEGATION_APPROVAL_MAGIC: [u8; 8] = *b"OMNILVDA";
/// Serialized approval payload version. Dusk is pre-deployment, so feature
/// iterations remain version 1. Increment only after a deployed payload format
/// must remain distinguishable from a new incompatible format.
pub const LEVERAGE_DELEGATION_APPROVAL_VERSION: u8 = 1;

/// Keeps shared, boxed swap preparation and the initial risk observation out
/// of the instruction handler's account-creation and token-CPI stack frame.
#[inline(never)]
pub(super) fn prepare_leverage_swap(
    market: &mut Market,
    request: SwapRequest,
    cash_policy: SwapCashPolicy,
) -> Result<PreparedLeverageSwap> {
    let current_slot = request.current_slot;
    let prepared = request.prepare_with_cash_policy(market, cash_policy)?;
    market.observe_current_risk(current_slot)?;
    Ok(prepared)
}

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

pub fn leverage_position_pda(
    market: Pubkey,
    owner: Pubkey,
    namespace_authority: Pubkey,
    position_id: Pubkey,
) -> Result<(Pubkey, u8)> {
    Pubkey::try_find_program_address(
        &[
            LEVERAGE_POSITION_SEED_PREFIX,
            market.as_ref(),
            owner.as_ref(),
            namespace_authority.as_ref(),
            position_id.as_ref(),
        ],
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
    event_authority: AccountInfo<'info>,
) -> Result<()> {
    let debt = (debt_mint.to_account_info(), debt_reserve_vault.to_account_info());
    let collateral = (
        collateral_mint.to_account_info(),
        collateral_reserve_vault.to_account_info(),
    );
    let assets = if debt_asset == MarketAsset::Base {
        [debt, collateral]
    } else {
        [collateral, debt]
    };
    settle_inline_hlp_accounts(
        market,
        futarchy_authority,
        assets,
        token_program,
        token_2022_program,
        remaining_accounts,
        layout,
        base_receipt,
        quote_receipt,
        interest_eligibility,
        event_authority,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn settle_inline_hlp_accounts<'info>(
    market: &mut Account<'info, Market>,
    futarchy_authority: &Account<'info, FutarchyAuthority>,
    asset_accounts: [(AccountInfo<'info>, AccountInfo<'info>); 2],
    token_program: &Program<'info, Token>,
    token_2022_program: &Program<'info, Token2022>,
    remaining_accounts: &[AccountInfo<'info>],
    layout: HlpSwapAccountLayout,
    base_receipt: HlpRebalanceReceipt,
    quote_receipt: HlpRebalanceReceipt,
    interest_eligibility: HlpYieldEligibility,
    event_authority: AccountInfo<'info>,
) -> Result<()> {
    for receipt in [base_receipt, quote_receipt] {
        if receipt.ylp_mint_amount == 0 && receipt.ylp_burn_amount == 0 && receipt.interest_paid == 0 {
            continue;
        }
        require_gte!(
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
        let (mint, reserve_vault) = asset_accounts[borrowed_asset.code() as usize].clone();
        let decimals = market.side(borrowed_asset).asset_decimals;
        let token_program_account = if *mint.owner == Token::id() {
            token_program.to_account_info()
        } else {
            require_keys_eq!(*mint.owner, Token2022::id(), ErrorCode::InvalidTokenProgram);
            token_2022_program.to_account_info()
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
        crate::instructions::accounting::emit_interest_paid(
            market,
            borrowed_asset,
            crate::events::DebtSource::Hlp,
            None,
            crate::state::ReferralInterestQuote::new(
                receipt.interest_paid,
                interest_credit,
                futarchy_authority.revenue_share.interest_bps,
                None,
            )?,
            0,
            event_authority.clone(),
        )?;
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
    /// Exact collateral amount sold by this full or partial close.
    pub collateral_amount: u64,
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
        collateral_amount: u64,
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
            collateral_amount,
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

fn delegated_callback_metas(
    accounts: &[AccountInfo<'_>],
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
        account_metas.push(AccountMeta {
            pubkey: account.key(),
            is_signer: false,
            is_writable: account.is_writable,
        });
    }
    Ok(account_metas)
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
    let account_metas = delegated_callback_metas(accounts, protected_accounts, writable_protected_accounts)?;
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
    expected_collateral_amount: u64,
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
        expected_collateral_amount,
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
    expected_collateral_amount: u64,
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
    require_eq!(
        approval.collateral_amount,
        expected_collateral_amount,
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

/// The fee a collateral transfer pays at `epoch`. Use it for exits, repayment,
/// and liquidation, which must not react to a change that has not happened.
pub fn leverage_collateral_fee(mint: &InterfaceAccount<Mint>, epoch: u64) -> Result<LeverageCollateralFee> {
    collateral_fee_schedule(&mint.to_account_info(), epoch, 0)
}

/// The effective fee plus any change already scheduled for a later epoch.
/// Token-2022 gives two epochs of notice before a new fee applies; checks that
/// issue debt or release collateral value it at the worse of the two.
pub fn leverage_collateral_admission_fee(mint: &InterfaceAccount<Mint>, epoch: u64) -> Result<LeverageCollateralFee> {
    collateral_fee_schedule(&mint.to_account_info(), epoch, u64::MAX)
}

/// An existing position gets its first notice epoch to repay or deleverage.
/// During the final notice epoch it may be liquidated while transfers still
/// pay the old fee, preventing an announced confiscatory fee from stranding
/// all collateral when it activates.
pub fn leverage_collateral_liquidation_fee(mint: &InterfaceAccount<Mint>, epoch: u64) -> Result<LeverageCollateralFee> {
    collateral_fee_schedule(&mint.to_account_info(), epoch, 1)
}

/// Validate both market mints before valuing stored lending contributions.
/// A fee authority can change either mint without touching the Market account.
pub fn lending_market_admission_fees(
    market: &Market,
    base_mint: &InterfaceAccount<Mint>,
    quote_mint: &InterfaceAccount<Mint>,
    epoch: u64,
) -> Result<crate::transitions::LendingCollateralFees> {
    require_keys_eq!(base_mint.key(), market.base_side.asset_mint, ErrorCode::InvalidMint);
    require_keys_eq!(quote_mint.key(), market.quote_side.asset_mint, ErrorCode::InvalidMint);
    Ok(crate::transitions::LendingCollateralFees {
        base: leverage_collateral_admission_fee(base_mint, epoch)?,
        quote: leverage_collateral_admission_fee(quote_mint, epoch)?,
    })
}

fn collateral_fee_schedule(
    mint_info: &AccountInfo,
    epoch: u64,
    pending_lookahead_epochs: u64,
) -> Result<LeverageCollateralFee> {
    if *mint_info.owner == Token::id() {
        return Ok(LeverageCollateralFee::default());
    }
    let mint_data = mint_info.try_borrow_data()?;
    let mint_state = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;
    if !mint_state
        .get_extension_types()?
        .contains(&ExtensionType::TransferFeeConfig)
    {
        return Ok(LeverageCollateralFee::default());
    }
    let config = mint_state.get_extension::<TransferFeeConfig>()?;
    let fee = LeverageCollateralFee::new(Some(*config.get_epoch_fee(epoch)));
    let pending_epoch = u64::from(config.newer_transfer_fee.epoch);
    let pending = (pending_epoch > epoch && pending_epoch - epoch <= pending_lookahead_epochs)
        .then_some(config.newer_transfer_fee);
    Ok(fee.with_pending(pending))
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
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
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
    interest_eligibility: HlpYieldEligibility,
    remaining_accounts: &[AccountInfo<'info>],
    event_authority: AccountInfo<'info>,
) -> Result<ReferralInterestAccrualReceipt> {
    let (fresh_gross, frozen_protocol_gross) = market
        .side_mut(debt_asset)
        .fees
        .public_interest_advance
        .take_payment(interest_paid)?;
    let transfer_gross = fresh_gross
        .checked_add(frozen_protocol_gross)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let interest_vault_balance_before = interest_vault.amount;
    if transfer_gross > 0 {
        let debt_token_program = token_program_for_mint(debt_mint, token_program, token_2022_program)?;
        transfer_checked_with_remaining_accounts(
            market.to_account_info(),
            debt_reserve_vault.to_account_info(),
            interest_vault.to_account_info(),
            debt_mint.to_account_info(),
            debt_token_program,
            transfer_gross,
            debt_mint.decimals,
            &[&generate_market_seeds!(market)[..]],
            remaining_accounts,
        )?;
        interest_vault.reload()?;
    }
    let interest_vault_credit = token_account_credit(interest_vault_balance_before, interest_vault)?;
    let frozen_protocol_credit = if transfer_gross == 0 {
        0
    } else {
        u64::try_from(crate::math::mul_div_u128(
            u128::from(interest_vault_credit),
            u128::from(frozen_protocol_gross),
            u128::from(transfer_gross),
        )?)
        .map_err(|_| ErrorCode::FeeMathOverflow)?
    };
    let fresh_credit = interest_vault_credit - frozen_protocol_credit;
    let fresh_protocol = crate::state::ReferralInterestQuote::new(
        fresh_gross,
        fresh_credit,
        futarchy_authority.revenue_share.interest_bps,
        None,
    )?
    .protocol_interest_revenue;
    let protocol_credit = fresh_protocol
        .checked_add(frozen_protocol_credit)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    let referral_receipt = crate::instructions::referral::accounting::accrue_referral_interest_with_protocol(
        expected_referral_partner,
        referral_interest_share_bps,
        futarchy_authority,
        referral_partner,
        referral_accrual,
        market.key(),
        debt_mint,
        interest_paid,
        interest_vault_credit,
        protocol_credit,
    )?;
    // Historical public growth is protected before the new cash credit.
    market.checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Base, interest_eligibility.base_hlp_ylp_shares)?;
    market.checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Quote, interest_eligibility.quote_hlp_ylp_shares)?;
    market.side_mut(debt_asset).record_interest_credit_allocation(
        interest_vault_credit,
        protocol_credit,
        futarchy_authority.protocol_auction_split,
        referral_receipt.quote.referral_amount,
        interest_eligibility.ylp_supply,
    )?;
    if market.has_active_hlp() && interest_vault_credit > protocol_credit {
        settle_new_public_hlp_income(
            market,
            futarchy_authority,
            token_program,
            token_2022_program,
            remaining_accounts,
            interest_eligibility,
            event_authority,
        )?;
        debt_reserve_vault.reload()?;
        interest_vault.reload()?;
    } else {
        market.checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Base, interest_eligibility.base_hlp_ylp_shares)?;
        market.checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Quote, interest_eligibility.quote_hlp_ylp_shares)?;
    }
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
    include!("../../tests/instructions/leverage/settlement.rs");
}

/// Fresh public repayment follows the same funding-before-yield policy as a
/// cash advance. The standard hLP prefix is followed by both mints/reserves.
#[allow(clippy::too_many_arguments)]
#[event]
pub struct HlpInterestAllocated {
    pub market: Pubkey,
    /// [hLP target side][income asset]. Historical holder growth is unchanged.
    pub published: [[u64; 2]; 2],
    pub capital_debit: [[u64; 2]; 2],
    pub capital_credit: [[u64; 2]; 2],
}

pub(crate) fn settle_new_public_hlp_income<'info>(
    market: &mut Account<'info, Market>,
    authority: &Account<'info, FutarchyAuthority>,
    token_program: &Program<'info, Token>,
    token_2022_program: &Program<'info, Token2022>,
    accounts: &[AccountInfo<'info>],
    eligibility: HlpYieldEligibility,
    event_authority: AccountInfo<'info>,
) -> Result<crate::transitions::liquidity::HlpNewInterestSettlement> {
    if !market.has_active_hlp() {
        return Ok(Default::default());
    }
    let layout = HlpSwapAccountLayout::try_from((&**market, accounts))?;
    let find = |key: Pubkey| {
        accounts
            .iter()
            .find(|a| a.key() == key)
            .cloned()
            .ok_or_else(|| error!(ErrorCode::NotEnoughAccounts))
    };
    let assets = [
        (
            find(market.base_side.asset_mint)?,
            find(market.base_side.reserve_vault)?,
        ),
        (
            find(market.quote_side.asset_mint)?,
            find(market.quote_side.reserve_vault)?,
        ),
    ];
    let clock = Clock::get()?;
    let fees = [
        collateral_fee_schedule(&assets[0].0, clock.epoch, 0)?,
        collateral_fee_schedule(&assets[1].0, clock.epoch, 0)?,
    ];
    let receipt = market.settle_new_hlp_public_interest(eligibility, fees, clock.slot)?;
    for t in 0..2 {
        for (i, (mint, reserve)) in assets.iter().enumerate() {
            let gross = receipt.reserve_debit[t][i];
            if gross == 0 {
                continue;
            }
            let interest = &accounts[if i == 0 {
                BASE_INTEREST_VAULT_INDEX
            } else {
                QUOTE_INTEREST_VAULT_INDEX
            }];
            let before = token_account_info_amount(reserve)?;
            let token = if *mint.owner == Token::id() {
                token_program.to_account_info()
            } else {
                require_keys_eq!(*mint.owner, Token2022::id(), ErrorCode::InvalidTokenProgram);
                token_2022_program.to_account_info()
            };
            transfer_checked_with_remaining_accounts(
                market.to_account_info(),
                interest.clone(),
                reserve.clone(),
                mint.clone(),
                token,
                gross,
                market
                    .side(if i == 0 { MarketAsset::Base } else { MarketAsset::Quote })
                    .asset_decimals,
                &[&generate_market_seeds!(market)[..]],
                layout.hook_accounts(accounts),
            )?;
            require_eq!(
                token_account_info_credit(before, reserve)?,
                receipt.reserve_credit[t][i],
                ErrorCode::BrokenInvariant
            );
        }
    }
    settle_inline_hlp_accounts(
        market,
        authority,
        assets.clone(),
        token_program,
        token_2022_program,
        accounts,
        layout,
        receipt.base_rebalance,
        receipt.quote_rebalance,
        eligibility,
        event_authority.clone(),
    )?;
    for (i, (_, reserve)) in assets.iter().enumerate() {
        require_reserve_custody(
            token_account_info_amount(reserve)?,
            market.side(if i == 0 { MarketAsset::Base } else { MarketAsset::Quote }),
        )?;
    }
    crate::instructions::accounting::emit_accounting_event(
        &HlpInterestAllocated {
            market: market.key(),
            published: receipt.published,
            capital_debit: receipt.reserve_debit,
            capital_credit: receipt.reserve_credit,
        },
        event_authority,
    )?;
    Ok(receipt)
}
