//! One-off, governance-attested net settlement of the fixed layout-17 market.
//! The exact old binary computes the reviewed manifest off-chain. This program
//! enforces funding, immutable beneficiaries, recipient safety and single use;
//! it does not independently prove the curve or fee calculation.
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    hash::{hash, Hasher},
    program_pack::Pack,
    system_program,
};
use spl_token::state::{Account as TokenAccount, AccountState, Mint};

mod freeze;
pub mod scope;
mod tokens;
pub use freeze::*;
use scope::*;
use tokens::*;

pub const RETIREMENT_SEED: &[u8] = b"legacy_retirement";
pub const ALL_OWNERS: u8 = 31;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, Default)]
pub struct LegacyPayout {
    pub quoted_base: u64,
    pub quoted_quote: u64,
    pub base: u64,
    pub quote: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct BeginLegacyRetirementArgs {
    pub source_slot: u64,
    pub bank_sha256: [u8; 32],
    pub quote_sha256: [u8; 32],
    pub owner_evidence_sha256: [[u8; 32]; 5],
    pub payouts: [LegacyPayout; 5],
}

#[account]
#[derive(InitSpace)]
pub struct LegacyRetirement {
    pub version: u8,
    pub bump: u8,
    pub completed: bool,
    pub cleaned_mask: u8,
    pub paid_mask: u8,
    pub source_slot: u64,
    pub manifest_sha256: [u8; 32],
    pub bank_sha256: [u8; 32],
    pub quote_sha256: [u8; 32],
    pub owner_evidence_sha256: [[u8; 32]; 5],
    pub payouts: [LegacyPayout; 5],
    pub custody: [u64; 2],
    pub remaining: [u64; 2],
}

/// Permanent replay/reinitialization barrier. Neither this account nor the
/// settlement state has a close instruction, including after final payment.
#[account]
pub struct RetiredLegacyMarket {
    pub retirement: Pubkey,
    pub version: u8,
}

#[event_cpi]
#[derive(Accounts)]
pub struct BeginLegacyRetirement<'info> {
    #[account(mut, address = AUTHORITY)]
    pub authority: Signer<'info>,
    #[account(address = PROGRAM_DATA, constraint = program_data.upgrade_authority_address == Some(AUTHORITY) @ RetirementError::Authority)]
    pub program_data: Account<'info, ProgramData>,
    #[account(seeds = [FREEZE_SEED, MARKET.as_ref()], bump = frozen.bump)]
    pub frozen: Box<Account<'info, FrozenLegacyMarket>>,
    /// CHECK: The exact legacy bytes are hash-bound before replacing their header.
    #[account(mut, address = MARKET, owner = crate::ID)]
    pub market: UncheckedAccount<'info>,
    #[account(init, payer = authority, space = 8 + LegacyRetirement::INIT_SPACE, seeds = [RETIREMENT_SEED, MARKET.as_ref()], bump)]
    pub retirement: Box<Account<'info, LegacyRetirement>>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, anchor_spl::token::Token>,
}

#[event_cpi]
#[derive(Accounts)]
pub struct ClearLegacyOwner<'info> {
    #[account(mut, seeds = [RETIREMENT_SEED, MARKET.as_ref()], bump = retirement.bump)]
    pub retirement: Box<Account<'info, LegacyRetirement>>,
    #[account(address = MARKET, has_one = retirement @ RetirementError::Scope)]
    pub market: Account<'info, RetiredLegacyMarket>,
    /// CHECK: Bound to the compiled owner before any rent is returned.
    #[account(mut)]
    pub owner: UncheckedAccount<'info>,
}

#[event_cpi]
#[derive(Accounts)]
pub struct PayLegacyRetirement<'info> {
    #[account(mut, seeds = [RETIREMENT_SEED, MARKET.as_ref()], bump = retirement.bump)]
    pub retirement: Box<Account<'info, LegacyRetirement>>,
    #[account(address = MARKET, has_one = retirement @ RetirementError::Scope)]
    pub market: Account<'info, RetiredLegacyMarket>,
    /// CHECK: Fixed address, mint, owner and authority safety are checked by the handler.
    #[account(mut, address = VAULTS[0])]
    pub base_vault: UncheckedAccount<'info>,
    /// CHECK: Same bounded validation as base_vault.
    #[account(mut, address = VAULTS[4])]
    pub quote_vault: UncheckedAccount<'info>,
    /// CHECK: The legacy mint address, token owner and decimals are verified.
    #[account(address = MINTS[0])]
    pub base_mint: UncheckedAccount<'info>,
    /// CHECK: Same mint validation as base_mint.
    #[account(address = MINTS[1])]
    pub quote_mint: UncheckedAccount<'info>,
    /// CHECK: Must be a safe token account owned by the fixed beneficiary.
    #[account(mut)]
    pub base_recipient: UncheckedAccount<'info>,
    /// CHECK: Same recipient validation as base_recipient.
    #[account(mut)]
    pub quote_recipient: UncheckedAccount<'info>,
    pub token_program: Program<'info, anchor_spl::token::Token>,
}

pub fn handle_begin<'info>(
    ctx: Context<'_, '_, '_, 'info, BeginLegacyRetirement<'info>>,
    args: BeginLegacyRetirementArgs,
) -> Result<()> {
    let clock = Clock::get()?;
    require!(
        args.source_slot >= ctx.accounts.program_data.slot && args.source_slot <= clock.slot,
        RetirementError::Evidence
    );
    require!(
        [args.bank_sha256, args.quote_sha256].iter().all(|v| *v != [0; 32])
            && args.owner_evidence_sha256.iter().all(|v| *v != [0; 32]),
        RetirementError::Evidence
    );
    let market = ctx.accounts.market.to_account_info();
    let data = market.try_borrow_data()?;
    require!(
        hash(&data).to_bytes() == ctx.accounts.frozen.market_sha256,
        RetirementError::Evidence
    );
    let freeze_slot = u64::from_le_bytes(
        ctx.accounts.frozen.clock_bytes[..8]
            .try_into()
            .map_err(|_| error!(RetirementError::Evidence))?,
    );
    require!(args.source_slot >= freeze_slot, RetirementError::Evidence);
    require!(
        data.len() >= 41 && data[..8] == hash(b"account:Market").to_bytes()[..8],
        RetirementError::Scope
    );
    drop(data);
    require!(ctx.remaining_accounts.len() == 10, RetirementError::Scope);
    let vault_amounts = ctx.accounts.frozen.vault_amounts;
    for (i, info) in ctx.remaining_accounts[..8].iter().enumerate() {
        require_keys_eq!(info.key(), VAULTS[i], RetirementError::Scope);
        let account = checked_token(info, MINTS[i / 4], MARKET)?;
        require!(
            account.amount >= vault_amounts[i] && info.is_writable,
            RetirementError::Funding
        );
    }
    for (i, mint) in MINTS.iter().enumerate() {
        checked_mint(&ctx.remaining_accounts[8 + i], *mint)?;
    }
    let custody = [sum_amounts(&vault_amounts[..4])?, sum_amounts(&vault_amounts[4..])?];
    let mut paid = [0u64; 2];
    for payout in args.payouts {
        require!(
            payout.base >= minimum_net(payout.quoted_base) && payout.quote >= minimum_net(payout.quoted_quote),
            RetirementError::Minimum
        );
        paid[0] = paid[0].checked_add(payout.base).ok_or(RetirementError::Math)?;
        paid[1] = paid[1].checked_add(payout.quote).ok_or(RetirementError::Math)?;
    }
    require!(paid[0] <= custody[0] && paid[1] <= custody[1], RetirementError::Funding);
    // Move only reviewed custody. Unsolicited post-snapshot excess is retained,
    // not used to reduce user entitlements or enlarge the treasury allocation.
    for (i, amount) in vault_amounts.iter().enumerate() {
        if i % 4 == 0 {
            continue;
        }
        transfer(
            &ctx.remaining_accounts[i],
            &ctx.remaining_accounts[8 + i / 4],
            &ctx.remaining_accounts[i / 4 * 4],
            &market,
            &ctx.accounts.token_program.to_account_info(),
            *amount,
        )?;
    }
    let state = &mut ctx.accounts.retirement;
    state.set_inner(LegacyRetirement {
        version: 1,
        bump: ctx.bumps.retirement,
        completed: false,
        cleaned_mask: 0,
        paid_mask: 0,
        source_slot: args.source_slot,
        manifest_sha256: hash(&args.try_to_vec()?).to_bytes(),
        bank_sha256: args.bank_sha256,
        quote_sha256: args.quote_sha256,
        owner_evidence_sha256: args.owner_evidence_sha256,
        payouts: args.payouts,
        custody,
        remaining: custody,
    });
    // Old normal binaries reject this discriminator after any attempted rollback.
    // Keep the allocation/rent permanently; no market reinitialization is possible.
    RetiredLegacyMarket {
        retirement: state.key(),
        version: 1,
    }
    .try_serialize(&mut &mut market.try_borrow_mut_data()?[..])?;
    emit_cpi!(LegacyRetirementBegun {
        manifest_sha256: state.manifest_sha256,
        source_slot: args.source_slot,
        custody
    });
    Ok(())
}

pub fn handle_clear<'info>(ctx: Context<'_, '_, '_, 'info, ClearLegacyOwner<'info>>, owner_index: u8) -> Result<()> {
    let index = checked_owner(owner_index)?;
    let state = &mut ctx.accounts.retirement;
    require!(
        !state.completed && state.cleaned_mask & (1 << index) == 0,
        RetirementError::Repeated
    );
    require_keys_eq!(ctx.accounts.owner.key(), OWNERS[index], RetirementError::Recipient);
    require!(
        ctx.remaining_accounts.len() == PERSONAL[index].len(),
        RetirementError::Scope
    );
    let mut digest = Hasher::default();
    digest.hash(b"DUSK_RETIREMENT_PERSONAL_V1");
    digest.hash(&[owner_index]);
    for (info, expected) in ctx.remaining_accounts.iter().zip(PERSONAL[index]) {
        require_keys_eq!(info.key(), expected.address, RetirementError::Scope);
        require!(info.is_writable && !info.executable, RetirementError::Scope);
        let data = info.try_borrow_data()?;
        let closed = info.owner == &system_program::ID && data.is_empty();
        require!(
            closed || info.owner == &crate::ID && data.get(..8) == Some(&expected.discriminator),
            RetirementError::Scope
        );
        digest.hash(info.key.as_ref());
        digest.hash(info.owner.as_ref());
        digest.hash(&[u8::from(info.executable)]);
        digest.hash(&(data.len() as u32).to_le_bytes());
        digest.hash(&data);
    }
    require!(
        digest.result().to_bytes() == state.owner_evidence_sha256[index],
        RetirementError::Evidence
    );
    let owner = ctx.accounts.owner.to_account_info();
    let mut rent = 0u64;
    for info in ctx.remaining_accounts {
        if info.owner != &crate::ID {
            continue;
        }
        let amount = info.lamports();
        let credited = owner.lamports().checked_add(amount).ok_or(RetirementError::Math)?;
        **owner.try_borrow_mut_lamports()? = credited;
        **info.try_borrow_mut_lamports()? = 0;
        info.assign(&system_program::ID);
        info.realloc(0, false)?;
        rent = rent.checked_add(amount).ok_or(RetirementError::Math)?;
    }
    state.cleaned_mask |= 1 << index;
    emit_cpi!(LegacyOwnerCleared {
        owner: OWNERS[index],
        rent_lamports: rent
    });
    Ok(())
}

pub fn handle_pay(ctx: Context<PayLegacyRetirement>, owner_index: u8) -> Result<()> {
    let index = checked_owner(owner_index)?;
    require!(
        !ctx.accounts.retirement.completed && ctx.accounts.retirement.paid_mask & (1 << index) == 0,
        RetirementError::Repeated
    );
    require!(
        ctx.accounts.retirement.cleaned_mask == ALL_OWNERS,
        RetirementError::Unsettled
    );
    let payout = ctx.accounts.retirement.payouts[index];
    pay_tokens(&ctx, OWNERS[index], [payout.base, payout.quote])?;
    let state = &mut ctx.accounts.retirement;
    state.remaining[0] = state.remaining[0]
        .checked_sub(payout.base)
        .ok_or(RetirementError::Funding)?;
    state.remaining[1] = state.remaining[1]
        .checked_sub(payout.quote)
        .ok_or(RetirementError::Funding)?;
    state.paid_mask |= 1 << index;
    emit_cpi!(LegacyRetirementPaid {
        owner: OWNERS[index],
        base: payout.base,
        quote: payout.quote,
        treasury: false
    });
    Ok(())
}

pub fn handle_finish(ctx: Context<PayLegacyRetirement>) -> Result<()> {
    let state = &ctx.accounts.retirement;
    require!(!state.completed, RetirementError::Repeated);
    require!(
        state.cleaned_mask == ALL_OWNERS && state.paid_mask == ALL_OWNERS,
        RetirementError::Unsettled
    );
    let remaining = state.remaining;
    pay_tokens(&ctx, TREASURY, remaining)?;
    ctx.accounts.retirement.remaining = [0; 2];
    ctx.accounts.retirement.completed = true;
    emit_cpi!(LegacyRetirementPaid {
        owner: TREASURY,
        base: remaining[0],
        quote: remaining[1],
        treasury: true
    });
    Ok(())
}

fn checked_owner(index: u8) -> Result<usize> {
    require!((index as usize) < OWNERS.len(), RetirementError::Scope);
    Ok(index as usize)
}

pub fn minimum_net(quote: u64) -> u64 {
    ((quote as u128 * 9900).div_ceil(10000)) as u64
}

#[event]
pub struct LegacyRetirementBegun {
    pub manifest_sha256: [u8; 32],
    pub source_slot: u64,
    pub custody: [u64; 2],
}
#[event]
pub struct LegacyOwnerCleared {
    pub owner: Pubkey,
    pub rent_lamports: u64,
}
#[event]
pub struct LegacyRetirementPaid {
    pub owner: Pubkey,
    pub base: u64,
    pub quote: u64,
    pub treasury: bool,
}

#[error_code]
pub enum RetirementError {
    #[msg("Retirement authority differs from the reviewed scope")]
    Authority,
    #[msg("Account or owner differs from the fixed legacy retirement scope")]
    Scope,
    #[msg("Retirement evidence differs from the frozen snapshot")]
    Evidence,
    #[msg("Reviewed custody cannot fund the immutable retirement claims")]
    Funding,
    #[msg("Net entitlement is below its reviewed 99% minimum")]
    Minimum,
    #[msg("Unsafe or incorrect retirement recipient")]
    Recipient,
    #[msg("All personal claims must be cleared before terminal payouts")]
    Unsettled,
    #[msg("Retirement action has already completed")]
    Repeated,
    #[msg("Retirement arithmetic overflow")]
    Math,
}

#[cfg(test)]
mod tests {
    include!("../tests/retirement.rs");
}
