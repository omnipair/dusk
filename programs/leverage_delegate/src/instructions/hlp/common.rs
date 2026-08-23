use super::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateHlpOrderArgs {
    pub order_id: u64,
    pub kind: u8,
    pub hlp_amount: u64,
    /// Stop Loss: principal NAV per hLP token in NAD. Stop Rate: opposite
    /// funding APR in NAD (NAD == 100% APR).
    pub trigger_nad: u64,
    pub min_target_amount_out: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct HlpOrderIdArgs {
    pub order_id: u64,
}

#[inline(never)]
pub(super) fn preview_hlp_order_trigger<'info>(
    accounts: &ExecuteHlpOrder<'info>,
    target_asset: MarketAsset,
    hlp_amount: u64,
) -> Result<dusk::instructions::HlpOrderTriggerPreview> {
    Ok(dusk::cpi::preview_hlp_order_trigger(
        CpiContext::new(
            accounts.dusk_program.to_account_info(),
            dusk::cpi::accounts::PreviewHlpOrderTrigger {
                market: accounts.market.to_account_info(),
            },
        ),
        dusk::instructions::PreviewHlpOrderTriggerArgs {
            target_asset: target_asset.code(),
            hlp_amount,
        },
    )?
    .get())
}

#[inline(never)]
pub(super) fn withdraw_hlp_order_position<'info>(
    accounts: &ExecuteHlpOrder<'info>,
    remaining_accounts: &[AccountInfo<'info>],
) -> Result<()> {
    let market_key = accounts.order.market;
    let owner_key = accounts.order.owner;
    let target_hlp_mint_key = accounts.order.target_hlp_mint;
    let order_id_bytes = accounts.order.order_id.to_le_bytes();
    let bump_seed = [accounts.order.bump];
    let authority_seeds = &[
        HLP_ORDER_SEED_PREFIX,
        market_key.as_ref(),
        owner_key.as_ref(),
        target_hlp_mint_key.as_ref(),
        &order_id_bytes,
        &bump_seed,
    ];
    dusk::cpi::withdraw_single_sided(
        CpiContext::new_with_signer(
            accounts.dusk_program.to_account_info(),
            dusk::cpi::accounts::WithdrawSingleSided {
                market: accounts.market.to_account_info(),
                futarchy_authority: accounts.futarchy_authority.to_account_info(),
                owner: accounts.order.to_account_info(),
                base_mint: accounts.base_mint.to_account_info(),
                quote_mint: accounts.quote_mint.to_account_info(),
                ylp_mint: accounts.ylp_mint.to_account_info(),
                target_hlp_mint: accounts.target_hlp_mint.to_account_info(),
                base_reserve_vault: accounts.base_reserve_vault.to_account_info(),
                quote_reserve_vault: accounts.quote_reserve_vault.to_account_info(),
                borrowed_interest_vault: accounts.borrowed_interest_vault.to_account_info(),
                owner_target_account: accounts.custody_target_account.to_account_info(),
                owner_hlp_account: accounts.custody_hlp_account.to_account_info(),
                hlp_ylp_account: accounts.hlp_ylp_account.to_account_info(),
                base_yield_account: accounts.base_yield_account.to_account_info(),
                quote_yield_account: accounts.quote_yield_account.to_account_info(),
                token_program: accounts.token_program.to_account_info(),
                token_2022_program: accounts.token_2022_program.to_account_info(),
                event_authority: accounts.dusk_event_authority.to_account_info(),
                program: accounts.dusk_program.to_account_info(),
            },
            &[&authority_seeds[..]],
        )
        .with_remaining_accounts(remaining_accounts.to_vec()),
        WithdrawSingleSidedArgs {
            hlp_amount: accounts.order.hlp_amount,
            min_target_amount_out: accounts.order.min_target_amount_out,
        },
    )
}

pub(super) fn validate_hlp_yield_account(
    account: &YieldAccount,
    owner: Pubkey,
    market: Pubkey,
    lp_mint: Pubkey,
    asset_mint: Pubkey,
) -> Result<()> {
    account.assert_account(owner, market, lp_mint, asset_mint, YieldTokenKind::Hlp)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn set_hlp_yield_recipient<'info>(
    dusk_program: AccountInfo<'info>,
    event_authority: AccountInfo<'info>,
    market: AccountInfo<'info>,
    owner: AccountInfo<'info>,
    lp_mint: AccountInfo<'info>,
    asset_mint: AccountInfo<'info>,
    yield_account: AccountInfo<'info>,
    recipient: Pubkey,
    signer: &[&[&[u8]]],
) -> Result<()> {
    dusk::cpi::set_yield_recipient(
        CpiContext::new_with_signer(
            dusk_program.clone(),
            dusk::cpi::accounts::SetYieldRecipient {
                market,
                owner,
                asset_mint,
                lp_mint,
                yield_account,
                event_authority,
                program: dusk_program,
            },
            signer,
        ),
        SetYieldRecipientArgs {
            token_kind: YieldTokenKind::Hlp,
            recipient,
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn claim_hlp_yield_if_available<'info>(
    accrued_swap_fee_amount: u64,
    accrued_interest_amount: u64,
    dusk_program: AccountInfo<'info>,
    event_authority: AccountInfo<'info>,
    market: AccountInfo<'info>,
    owner: AccountInfo<'info>,
    asset_mint: AccountInfo<'info>,
    lp_mint: AccountInfo<'info>,
    owner_lp_account: AccountInfo<'info>,
    reserve_vault: AccountInfo<'info>,
    interest_vault: AccountInfo<'info>,
    recipient_asset_account: AccountInfo<'info>,
    yield_account: AccountInfo<'info>,
    token_program: AccountInfo<'info>,
    token_2022_program: AccountInfo<'info>,
    signer: &[&[&[u8]]],
    remaining_accounts: &[AccountInfo<'info>],
) -> Result<()> {
    if accrued_swap_fee_amount == 0 && accrued_interest_amount == 0 {
        return Ok(());
    }
    dusk::cpi::harvest(
        CpiContext::new_with_signer(
            dusk_program.clone(),
            dusk::cpi::accounts::Harvest {
                market,
                owner,
                asset_mint,
                lp_mint,
                owner_lp_account,
                reserve_vault,
                interest_vault,
                recipient_asset_account,
                yield_account,
                token_program,
                token_2022_program,
                event_authority,
                program: dusk_program,
            },
            signer,
        )
        .with_remaining_accounts(remaining_accounts.to_vec()),
        HarvestArgs {
            token_kind: YieldTokenKind::Hlp,
        },
    )
}

pub(super) fn validate_hlp_order_kind(kind: u8) -> Result<()> {
    require!(
        kind == HLP_ORDER_KIND_STOP_LOSS || kind == HLP_ORDER_KIND_STOP_RATE,
        LeverageDelegateError::InvalidOrder
    );
    Ok(())
}

pub(super) fn hlp_order_trigger_met(
    kind: u8,
    principal_nav_nad: u64,
    funding_apr_nad: u128,
    trigger_nad: u64,
) -> Result<bool> {
    match kind {
        HLP_ORDER_KIND_STOP_LOSS => Ok(principal_nav_nad <= trigger_nad),
        HLP_ORDER_KIND_STOP_RATE => Ok(funding_apr_nad >= trigger_nad as u128),
        _ => err!(LeverageDelegateError::InvalidOrder),
    }
}
