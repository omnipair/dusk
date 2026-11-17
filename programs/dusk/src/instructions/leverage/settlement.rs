use crate::transitions::amm::SwapRequest;
use anchor_lang::{
    prelude::*,
    solana_program::instruction::{AccountMeta, Instruction},
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
        require_supported_asset_mint, validate_owner_asset_account, validate_side_vault_accounts, HlpSwapAccountLayout,
        BASE_HLP_SRLP_VAULT_INDEX, HLP_SRLP_MINT_INDEX, HLP_SWAP_ACCOUNT_PREFIX_LEN, QUOTE_HLP_SRLP_VAULT_INDEX,
    },
    instructions::referral::accounting::ReferralInterestAccrualReceipt,
    state::{FutarchyAuthority, Market, MarketAsset, ReferralAccrual, ReferralPartner},
    token::{get_transfer_fee_for_epoch, invoke_signed_reusing_instruction, token_burn, token_mint_to},
    transitions::{
        HlpRebalanceReceipt, LeverageCollateralFee, LeverageSwapFeeCredit, LeverageSwapQuote, PreparedLeverageSwap,
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
pub(crate) fn prepare_leverage_swap(
    market: &mut Market,
    request: SwapRequest,
    cash_policy: SwapCashPolicy,
) -> Result<PreparedLeverageSwap> {
    let current_slot = request.current_slot;
    let prepared = request.prepare_with_cash_policy(market, cash_policy)?;
    market.observe_current_risk(current_slot)?;
    Ok(prepared)
}

/// Validates the canonical Market PDA and its layout outside Anchor's
/// generated account parser. Keeping the seed-array construction out of
/// `try_accounts` avoids overlapping it with the large Market deserialization
/// frame.
pub fn validate_leverage_market_pda(market: &Market, market_key: Pubkey) -> Result<()> {
    market.assert_current_version()?;
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

/// Mint or burn the vault-held srLP an hLP plan moved and report each funding
/// payment. The protocol share the plan moved to `protocol_pending` is left
/// for the instruction to book once, after any referral is known.
#[allow(clippy::too_many_arguments)]
pub fn settle_inline_leverage_hlp<'info>(
    market: &mut Account<'info, Market>,
    token_2022_program: &Program<'info, Token2022>,
    remaining_accounts: &[AccountInfo<'info>],
    layout: HlpSwapAccountLayout,
    base_receipt: HlpRebalanceReceipt,
    quote_receipt: HlpRebalanceReceipt,
    event_authority: AccountInfo<'info>,
) -> Result<()> {
    settle_inline_hlp_accounts(
        market,
        token_2022_program,
        remaining_accounts,
        layout,
        base_receipt,
        quote_receipt,
        event_authority.clone(),
    )?;
    if market.base_hlp_vault.hlp_supply > 0 || market.quote_hlp_vault.hlp_supply > 0 {
        crate::instructions::accounting::emit_hlp_rebalanced(
            market,
            base_receipt,
            quote_receipt,
            Clock::get()?.slot,
            event_authority,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
/// Settle an hLP plan's token side. It never books protocol interest: a
/// booking without the referral would empty `protocol_pending` before the
/// instruction carves the referral from the payment's protocol share.
pub(crate) fn settle_inline_hlp_accounts<'info>(
    market: &mut Account<'info, Market>,
    token_2022_program: &Program<'info, Token2022>,
    remaining_accounts: &[AccountInfo<'info>],
    layout: HlpSwapAccountLayout,
    base_receipt: HlpRebalanceReceipt,
    quote_receipt: HlpRebalanceReceipt,
    event_authority: AccountInfo<'info>,
) -> Result<()> {
    for receipt in [base_receipt, quote_receipt] {
        if receipt.srlp_mint_amount == 0 && receipt.srlp_burn_amount == 0 && receipt.interest_paid == 0 {
            continue;
        }
        if receipt.srlp_mint_amount > 0 || receipt.srlp_burn_amount > 0 {
            require_gte!(
                layout.prefix_len,
                HLP_SWAP_ACCOUNT_PREFIX_LEN,
                ErrorCode::NotEnoughAccounts
            );
            require!(
                receipt.srlp_mint_amount == 0 || receipt.srlp_burn_amount == 0,
                ErrorCode::BrokenInvariant
            );
            let srlp_vault_index = if receipt.target_asset == MarketAsset::Base {
                BASE_HLP_SRLP_VAULT_INDEX
            } else {
                QUOTE_HLP_SRLP_VAULT_INDEX
            };
            let market_seeds = generate_market_seeds!(market);
            let signer_seeds = [&market_seeds[..]];
            if receipt.srlp_mint_amount > 0 {
                token_mint_to(
                    market.to_account_info(),
                    token_2022_program.to_account_info(),
                    remaining_accounts[HLP_SRLP_MINT_INDEX].clone(),
                    remaining_accounts[srlp_vault_index].clone(),
                    receipt.srlp_mint_amount,
                    &signer_seeds,
                )?;
            }
            if receipt.srlp_burn_amount > 0 {
                token_burn(
                    market.to_account_info(),
                    token_2022_program.to_account_info(),
                    remaining_accounts[HLP_SRLP_MINT_INDEX].clone(),
                    remaining_accounts[srlp_vault_index].clone(),
                    receipt.srlp_burn_amount,
                    &signer_seeds,
                )?;
            }
        }
        if receipt.interest_paid > 0 {
            crate::instructions::accounting::emit_interest_paid(
                market,
                receipt.target_asset.opposite(),
                crate::events::DebtSource::Hlp,
                None,
                crate::state::ReferralInterestQuote::with_protocol_revenue(
                    receipt.interest_paid,
                    receipt.protocol_paid,
                    None,
                )?,
                0,
                event_authority.clone(),
            )?;
        }
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

pub struct DelegatedCallbackBuffers<'info> {
    instruction: Instruction,
    account_infos: Vec<AccountInfo<'info>>,
}

impl Default for DelegatedCallbackBuffers<'_> {
    fn default() -> Self {
        Self {
            instruction: Instruction {
                program_id: Pubkey::default(),
                accounts: Vec::new(),
                data: Vec::new(),
            },
            account_infos: Vec::new(),
        }
    }
}

impl DelegatedCallbackBuffers<'_> {
    pub fn with_account_capacity(account_capacity: usize) -> Self {
        Self {
            instruction: Instruction {
                program_id: Pubkey::default(),
                accounts: Vec::with_capacity(account_capacity),
                data: Vec::new(),
            },
            account_infos: Vec::with_capacity(account_capacity + 1),
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(unexpected_cfgs)]
#[inline(never)]
pub fn invoke_delegated_approval_callback<'info>(
    buffers: &mut DelegatedCallbackBuffers<'info>,
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
    require!(!data.is_empty(), ErrorCode::InvalidLeverageDelegation);
    require!(delegated_program.executable, ErrorCode::InvalidLeverageDelegation);
    buffers.instruction.program_id = delegated_program.key();
    buffers.instruction.accounts.clear();
    for (index, account) in accounts.iter().enumerate() {
        for prior in accounts.iter().take(index) {
            require_keys_neq!(account.key(), prior.key(), ErrorCode::InvalidLeverageDelegation);
        }
        let is_protected = protected_accounts.contains(account.key);
        let is_writable_protected = writable_protected_accounts.contains(account.key);
        let meta = if is_protected && !is_writable_protected {
            AccountMeta::new_readonly(account.key(), false)
        } else {
            AccountMeta {
                pubkey: account.key(),
                is_signer: false,
                is_writable: account.is_writable,
            }
        };
        buffers.instruction.accounts.push(meta);
    }
    buffers.instruction.data = data;
    buffers.account_infos.clear();
    buffers.account_infos.push(delegated_program.to_account_info());
    buffers.account_infos.extend(accounts.iter().cloned());
    invoke_signed_reusing_instruction(&mut buffers.instruction, &buffers.account_infos, &[])?;

    #[cfg(target_os = "solana")]
    {
        // The approval has a fixed 222-byte wire shape. Read it directly into
        // bounded stack storage so validating a callback does not retain a
        // heap copy for the rest of a large composite instruction.
        let mut data = [0u8; 256];
        let mut program_id = Pubkey::default();
        let size = unsafe {
            anchor_lang::solana_program::syscalls::sol_get_return_data(
                data.as_mut_ptr(),
                data.len() as u64,
                &mut program_id,
            )
        };
        require!(
            size > 0 && size as usize <= data.len(),
            ErrorCode::InvalidLeverageDelegation
        );
        return validate_delegation_approval(
            program_id,
            &data[..size as usize],
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
        );
    }

    #[cfg(not(target_os = "solana"))]
    {
        let (program_id, data) =
            anchor_lang::solana_program::program::get_return_data().ok_or(ErrorCode::InvalidLeverageDelegation)?;
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
    // The approval has only fixed-size Borsh fields. Compare its canonical
    // 222-byte wire image in one pass instead of deserializing and branching
    // over every field at the end of an already compute-heavy close.
    let mut expected = [0u8; 222];
    expected[..8].copy_from_slice(&LEVERAGE_DELEGATION_APPROVAL_MAGIC);
    expected[8] = LEVERAGE_DELEGATION_APPROVAL_VERSION;
    expected[9..13].copy_from_slice(&expected_action.to_le_bytes());
    expected[13..45].copy_from_slice(expected_market.as_ref());
    expected[45..77].copy_from_slice(expected_owner.as_ref());
    expected[77..109].copy_from_slice(expected_position.as_ref());
    expected[109..141].copy_from_slice(expected_delegation.as_ref());
    expected[141] = expected_debt_asset.code();
    expected[142..174].copy_from_slice(expected_recipient_token_account.as_ref());
    expected[174..206].copy_from_slice(expected_output_mint.as_ref());
    expected[206..214].copy_from_slice(&expected_collateral_amount.to_le_bytes());
    expected[214..222].copy_from_slice(&expected_output_amount.to_le_bytes());
    require!(data == expected, ErrorCode::InvalidLeverageDelegation);
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

/// Record a borrower's interest payment: the referral share from this
/// payment's protocol share, then book all pending protocol interest, the
/// debt asset's with that referral. The interest itself stays in reserve
/// cash; no token moves. Nothing earlier in the instruction books.
#[allow(clippy::too_many_arguments)]
pub fn record_interest_paid<'info>(
    market: &mut Account<'info, Market>,
    debt_asset: MarketAsset,
    debt_mint: &InterfaceAccount<'info, Mint>,
    futarchy_authority: &Account<'info, FutarchyAuthority>,
    expected_referral_partner: Pubkey,
    referral_interest_share_bps: u16,
    referral_partner: Option<&Account<'info, ReferralPartner>>,
    referral_accrual: Option<&mut Account<'info, ReferralAccrual>>,
    interest_paid: u64,
    protocol_paid: u64,
) -> Result<ReferralInterestAccrualReceipt> {
    let referral_receipt = crate::instructions::referral::accounting::accrue_referral_interest_with_protocol(
        expected_referral_partner,
        referral_interest_share_bps,
        futarchy_authority,
        referral_partner,
        referral_accrual,
        market.key(),
        debt_mint,
        interest_paid,
        protocol_paid,
    )?;
    market.book_paid_protocol_interest(
        debt_asset,
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
    include!("../../tests/instructions/leverage/settlement.rs");
}
