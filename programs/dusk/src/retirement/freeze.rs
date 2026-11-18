use super::*;

pub const FREEZE_SEED: &[u8] = b"legacy_freeze";

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct FrozenReceiptAccount {
    pub address: Pubkey,
    pub token_program: Pubkey,
    pub data: Vec<u8>,
}

#[account]
#[derive(InitSpace)]
pub struct FrozenLegacyMarket {
    pub bump: u8,
    pub market_sha256: [u8; 32],
    pub clock_bytes: [u8; 40],
    pub vault_amounts: [u64; 8],
    #[max_len(8192)]
    pub receipt_snapshot: Vec<u8>,
}

#[derive(Accounts)]
pub struct FreezeLegacyMarket<'info> {
    #[account(mut, address = AUTHORITY)]
    pub authority: Signer<'info>,
    #[account(address = PROGRAM_DATA, constraint = program_data.upgrade_authority_address == Some(AUTHORITY) @ RetirementError::Authority)]
    pub program_data: Account<'info, ProgramData>,
    /// CHECK: Exact legacy market, checked before storing its immutable digest.
    #[account(address = MARKET, owner = crate::ID)]
    pub market: UncheckedAccount<'info>,
    #[account(init, payer = authority, space = 8 + FrozenLegacyMarket::INIT_SPACE, seeds = [FREEZE_SEED, MARKET.as_ref()], bump)]
    pub frozen: Box<Account<'info, FrozenLegacyMarket>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AbortStaleLegacyFreeze<'info> {
    #[account(mut, address = AUTHORITY)]
    pub authority: Signer<'info>,
    #[account(mut, close = authority, seeds = [FREEZE_SEED, MARKET.as_ref()], bump = frozen.bump)]
    pub frozen: Box<Account<'info, FrozenLegacyMarket>>,
    /// CHECK: Cannot reset a freeze once a funded settlement exists.
    #[account(seeds = [RETIREMENT_SEED, MARKET.as_ref()], bump)]
    pub retirement: UncheckedAccount<'info>,
    /// CHECK: A restored normal binary must actually have changed the market.
    #[account(address = MARKET, owner = crate::ID)]
    pub market: UncheckedAccount<'info>,
}

pub fn handle_freeze<'info>(ctx: Context<'_, '_, '_, 'info, FreezeLegacyMarket<'info>>) -> Result<()> {
    let data = ctx.accounts.market.try_borrow_data()?;
    require!(
        data.len() >= 41 && data[..8] == hash(b"account:Market").to_bytes()[..8],
        RetirementError::Scope
    );
    let market_hash = hash(&data).to_bytes();
    drop(data);
    require!(
        ctx.remaining_accounts.len() == 4 + RECEIPT_ACCOUNTS.len() + VAULTS.len(),
        RetirementError::Scope
    );
    let mut snapshot = Vec::new();
    let mut supplies = [0u64; 4];
    let mut totals = [0u64; 4];
    let token_program = anchor_spl::token_2022::ID;
    for (index, info) in ctx.remaining_accounts[..4 + RECEIPT_ACCOUNTS.len()].iter().enumerate() {
        require_keys_eq!(*info.owner, token_program, RetirementError::Scope);
        require!(!info.executable, RetirementError::Scope);
        let data = info.try_borrow_data()?;
        if index < 4 {
            require_keys_eq!(info.key(), RECEIPT_MINTS[index], RetirementError::Scope);
            let mint = Mint::unpack(data.get(..Mint::LEN).ok_or(RetirementError::Scope)?)?;
            supplies[index] = mint.supply;
        } else {
            require_keys_eq!(info.key(), RECEIPT_ACCOUNTS[index - 4], RetirementError::Scope);
            let token = TokenAccount::unpack(data.get(..TokenAccount::LEN).ok_or(RetirementError::Scope)?)?;
            let side = RECEIPT_MINTS
                .iter()
                .position(|mint| *mint == token.mint)
                .ok_or(RetirementError::Scope)?;
            require!(
                token.amount == 0 || OWNERS.contains(&token.owner) || RECEIPT_CUSTODY.contains(&token.owner),
                RetirementError::Scope
            );
            totals[side] = totals[side].checked_add(token.amount).ok_or(RetirementError::Math)?;
        }
        snapshot.push(FrozenReceiptAccount {
            address: info.key(),
            token_program: *info.owner,
            data: data.to_vec(),
        });
    }
    require!(supplies == totals, RetirementError::Unsettled);
    let mut vault_amounts = [0u64; 8];
    for (index, info) in ctx.remaining_accounts[4 + RECEIPT_ACCOUNTS.len()..].iter().enumerate() {
        require_keys_eq!(info.key(), VAULTS[index], RetirementError::Scope);
        vault_amounts[index] = checked_token(info, MINTS[index / 4], MARKET)?.amount;
    }
    let bytes = snapshot.try_to_vec()?;
    require!(bytes.len() <= 8192, RetirementError::Scope);
    let clock = Clock::get()?;
    require!(clock.slot >= ctx.accounts.program_data.slot, RetirementError::Evidence);
    let clock_bytes = bincode::serialize(&clock)
        .map_err(|_| error!(RetirementError::Evidence))?
        .try_into()
        .map_err(|_| error!(RetirementError::Evidence))?;
    ctx.accounts.frozen.set_inner(FrozenLegacyMarket {
        bump: ctx.bumps.frozen,
        market_sha256: market_hash,
        clock_bytes,
        vault_amounts,
        receipt_snapshot: bytes,
    });
    emit!(LegacyMarketFrozen {
        slot: clock.slot,
        market_sha256: market_hash,
        receipts_sha256: hash(&ctx.accounts.frozen.receipt_snapshot).to_bytes()
    });
    Ok(())
}

pub fn handle_abort_stale_freeze(ctx: Context<AbortStaleLegacyFreeze>) -> Result<()> {
    require!(
        ctx.accounts.retirement.owner == &system_program::ID && ctx.accounts.retirement.data_is_empty(),
        RetirementError::Repeated
    );
    let data = ctx.accounts.market.try_borrow_data()?;
    require!(
        data.len() >= 41 && data[..8] == hash(b"account:Market").to_bytes()[..8],
        RetirementError::Scope
    );
    // Resetting an unchanged frozen market would move the interest cutoff and
    // charge users for review time. Only a genuinely stale freeze is resettable.
    require!(
        hash(&data).to_bytes() != ctx.accounts.frozen.market_sha256,
        RetirementError::Evidence
    );
    Ok(())
}

#[event]
pub struct LegacyMarketFrozen {
    pub slot: u64,
    pub market_sha256: [u8; 32],
    pub receipts_sha256: [u8; 32],
}
