use anchor_lang::{
    prelude::*,
    solana_program::{instruction::Instruction, program::invoke_signed},
    Event,
};

use crate::{
    events::{BorrowInterestAccrued, BorrowInterestPaid, DebtSource},
    state::{Market, MarketAsset, ReferralInterestQuote},
    transitions::lending::accrue_side,
};

/// Emit through the same self-CPI format as `emit_cpi!`, from shared account
/// preparation where an Anchor `Context` is not available. Callers supply
/// the seed-validated event authority from their `#[event_cpi]` accounts.
pub(crate) fn emit_accounting_event<'info>(event: &impl Event, authority: AccountInfo<'info>) -> Result<()> {
    let (_, bump) = Pubkey::find_program_address(&[b"__event_authority"], &crate::ID);
    let data = [anchor_lang::event::EVENT_IX_TAG_LE, event.data().as_slice()].concat();
    let instruction =
        Instruction::new_with_bytes(crate::ID, &data, vec![AccountMeta::new_readonly(authority.key(), true)]);
    invoke_signed(&instruction, &[authority], &[&[b"__event_authority", &[bump]]])?;
    Ok(())
}

pub(crate) fn emit_interest_paid<'info>(
    market: &Account<'info, Market>,
    asset: MarketAsset,
    source: DebtSource,
    position: Option<Pubkey>,
    quote: ReferralInterestQuote,
    caller_bounty: u64,
    event_authority: AccountInfo<'info>,
) -> Result<()> {
    if quote.interest_paid == 0 {
        return Ok(());
    }
    emit_accounting_event(
        &BorrowInterestPaid {
            market: market.key(),
            asset_mint: market.side(asset).asset_mint,
            asset_side: asset.code(),
            source,
            position,
            interest_paid: quote.interest_paid,
            interest_vault_credit: quote.interest_vault_credit,
            protocol_interest_revenue: quote.protocol_interest_revenue,
            referral_amount: quote.referral_amount,
            caller_bounty,
            slot: Clock::get()?.slot,
        },
        event_authority,
    )
}

/// Checkpoint a real market account, before changing any debt shares. Scratch
/// quotes use the quiet domain transition, so they never publish accruals.
pub(crate) fn accrue_market_interest<'info>(
    market: &mut Account<'info, Market>,
    current_slot: u64,
    event_authority: AccountInfo<'info>,
) -> Result<()> {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        if let Some(receipt) = accrue_side::<true>(market, asset, current_slot)? {
            emit_accounting_event(
                &BorrowInterestAccrued {
                    market: market.key(),
                    asset_mint: market.side(asset).asset_mint,
                    asset_side: receipt.asset.code(),
                    from_slot: receipt.from_slot,
                    to_slot: receipt.to_slot,
                    borrow_index_before_nad: receipt.index_before,
                    borrow_index_after_nad: receipt.index_after,
                    credit_interest: receipt.credit_interest,
                    margin_interest: receipt.margin_interest,
                    hlp_interest: receipt.hlp_interest,
                },
                event_authority.clone(),
            )?;
        }
    }
    Ok(())
}
