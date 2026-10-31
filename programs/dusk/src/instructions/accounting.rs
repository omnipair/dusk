use anchor_lang::{prelude::*, solana_program::instruction::Instruction, Event};

use crate::{
    events::{BorrowInterestAccrued, BorrowInterestPaid, DebtSource, HlpRebalanced},
    state::{Market, MarketAsset, ReferralInterestQuote},
    token::invoke_signed_reusing_instruction,
    transitions::{lending::accrue_side, HlpRebalanceReceipt},
};

/// Emit through the same self-CPI format as `emit_cpi!`, from shared account
/// preparation where an Anchor `Context` is not available. Callers supply
/// the seed-validated event authority from their `#[event_cpi]` accounts.
pub(crate) fn emit_accounting_event<'info, E: Event>(event: &E, authority: AccountInfo<'info>) -> Result<()> {
    let mut instruction = Instruction {
        program_id: Pubkey::default(),
        accounts: Vec::new(),
        data: Vec::new(),
    };
    emit_cpi_event_with_instruction(event, authority, &mut instruction)
}

/// Reuses one self-CPI instruction buffer across several events in a large
/// composite instruction. The SBF bump allocator does not reclaim the vectors
/// allocated by consecutive event emissions during the same invocation.
pub(crate) fn emit_cpi_event_with_instruction<'info, E: Event>(
    event: &E,
    authority: AccountInfo<'info>,
    instruction: &mut Instruction,
) -> Result<()> {
    let (_, bump) = Pubkey::find_program_address(&[b"__event_authority"], &crate::ID);
    instruction.program_id = crate::ID;
    instruction.accounts.clear();
    instruction
        .accounts
        .push(AccountMeta::new_readonly(authority.key(), true));
    instruction.data.clear();
    instruction.data.extend_from_slice(anchor_lang::event::EVENT_IX_TAG_LE);
    instruction.data.extend_from_slice(E::DISCRIMINATOR);
    event.serialize(&mut instruction.data)?;
    invoke_signed_reusing_instruction(instruction, &[authority], &[&[b"__event_authority", &[bump]]])?;
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
    let mut instruction = Instruction {
        program_id: Pubkey::default(),
        accounts: Vec::new(),
        data: Vec::new(),
    };
    emit_interest_paid_with_instruction(
        market,
        asset,
        source,
        position,
        quote,
        caller_bounty,
        event_authority,
        &mut instruction,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_interest_paid_with_instruction<'info>(
    market: &Account<'info, Market>,
    asset: MarketAsset,
    source: DebtSource,
    position: Option<Pubkey>,
    quote: ReferralInterestQuote,
    caller_bounty: u64,
    event_authority: AccountInfo<'info>,
    instruction: &mut Instruction,
) -> Result<()> {
    if quote.interest_paid == 0 {
        return Ok(());
    }
    emit_cpi_event_with_instruction(
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
        instruction,
    )
}

/// Publish both vault-owned yLP changes together, including debt-only hedge
/// corrections that have no token mint or burn inner instruction.
pub(crate) fn emit_hlp_rebalanced<'info>(
    market: &Account<'info, Market>,
    base: HlpRebalanceReceipt,
    quote: HlpRebalanceReceipt,
    slot: u64,
    event_authority: AccountInfo<'info>,
) -> Result<()> {
    if let Some(event) = HlpRebalanced::from_receipts(market.key(), market, base, quote, slot)? {
        emit_accounting_event(&event, event_authority)?;
    }
    Ok(())
}

/// Checkpoint a real market account, before changing any debt shares. Scratch
/// quotes use the quiet domain transition, so they never publish accruals.
pub(crate) fn accrue_market_interest<'info>(
    market: &mut Account<'info, Market>,
    current_slot: u64,
    event_authority: AccountInfo<'info>,
) -> Result<()> {
    let key = market.key();
    accrue_market_interest_data(market, key, current_slot, event_authority)
}

pub(crate) fn accrue_market_interest_data<'info>(
    market: &mut Market,
    market_key: Pubkey,
    current_slot: u64,
    event_authority: AccountInfo<'info>,
) -> Result<()> {
    market.base_side.fees.public_interest_advance.require_settled()?;
    market.quote_side.fees.public_interest_advance.require_settled()?;
    if market.debt.base_last_accrual_slot == current_slot && market.debt.quote_last_accrual_slot == current_slot {
        return market.accrue_interest_to_slot(current_slot);
    }
    let signals = market.tranche_coverage_signals()?;
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        if let Some(receipt) = accrue_side::<true>(market, asset, current_slot, signals[asset.code() as usize])? {
            emit_accounting_event(
                &BorrowInterestAccrued {
                    market: market_key,
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
