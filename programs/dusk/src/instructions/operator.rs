use crate::{account::get_size_with_discriminator, constants::*, errors::ErrorCode, state::*};
use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct AnnounceOperatorChangeArgs {
    pub nonce: u64,
    pub expected_epoch: u64,
    pub updates: Vec<MarketParameterUpdate>,
    pub next_operator: Option<Pubkey>,
    pub revenue_recipient: Option<Pubkey>,
    pub interest_share_bps: Option<u16>,
    pub metadata: ProposalMetadataV1,
}

#[event]
pub struct OperatorChanged {
    pub market: Pubkey,
    pub operator: Pubkey,
    pub epoch: u64,
}
#[event]
pub struct OperatorChangeAnnounced {
    pub market: Pubkey,
    pub proposal: Pubkey,
    pub operator: Pubkey,
    pub nonce: u64,
    pub digest: [u8; 32],
    pub execute_after_slot: u64,
    pub deadline_slot: u64,
    pub epoch: u64,
    pub policy_version: u64,
    pub updates: Vec<MarketParameterUpdate>,
    pub next_operator: Option<Pubkey>,
    pub revenue_recipient: Option<Pubkey>,
    pub interest_share_bps: Option<u16>,
    pub metadata: ProposalMetadataV1,
    pub family_revisions: [u64; PARAMETER_FAMILY_COUNT],
}
#[event]
pub struct OperatorChangeResolved {
    pub market: Pubkey,
    pub proposal: Pubkey,
    pub executed: bool,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: AnnounceOperatorChangeArgs)]
pub struct AnnounceOperatorChange<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub operator: Signer<'info>,
    #[account(mut, seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
    #[account(init, payer=payer, space=get_size_with_discriminator::<OperatorProposal>(), seeds=[OPERATOR_PROPOSAL_SEED, market.key().as_ref(), &args.nonce.to_le_bytes()], bump)]
    pub proposal: Box<Account<'info, OperatorProposal>>,
    pub system_program: Program<'info, System>,
}

impl AnnounceOperatorChange<'_> {
    pub fn handle(ctx: Context<Self>, args: AnnounceOperatorChangeArgs) -> Result<()> {
        let market = &mut ctx.accounts.market;
        market.assert_current_version()?;
        market.operator.assert_authority(ctx.accounts.operator.key())?;
        require_eq!(
            args.expected_epoch,
            market.operator.epoch,
            ErrorCode::OperatorEpochMismatch
        );
        require_eq!(args.nonce, market.operator.next_nonce, ErrorCode::OperatorNonceMismatch);
        require!(
            !args.updates.is_empty()
                || args.next_operator.is_some()
                || args.revenue_recipient.is_some()
                || args.interest_share_bps.is_some(),
            ErrorCode::ParameterUpdateNotMeaningful
        );
        if let Some(bps) = args.interest_share_bps {
            require!(
                bps <= MAX_OPERATOR_INTEREST_SHARE_BPS,
                ErrorCode::OperatorPolicyExceeded
            );
        }
        if let Some(recipient) = args.revenue_recipient {
            require!(recipient != Pubkey::default(), ErrorCode::InvalidOperator);
        }
        args.metadata.validate()?;
        let clock = Clock::get()?;
        market.assert_launch_settings_unchanged_after_start(&args.updates, clock.unix_timestamp)?;
        market.validate_operator_updates(&args.updates)?;
        let proposal = &mut ctx.accounts.proposal;
        proposal.market = market.key();
        proposal.operator = ctx.accounts.operator.key();
        proposal.epoch = market.operator.epoch;
        proposal.nonce = args.nonce;
        proposal.policy_version = OPERATOR_POLICY_VERSION;
        proposal.updates = args.updates;
        proposal.next_operator = args.next_operator;
        proposal.revenue_recipient = args.revenue_recipient;
        proposal.interest_share_bps = args.interest_share_bps;
        proposal.metadata = args.metadata;
        proposal.family_revisions = market.parameter_revisions;
        proposal.announced_at_slot = clock.slot;
        proposal.execute_after_slot = clock
            .slot
            .checked_add(OPERATOR_NOTICE_SLOTS)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        proposal.deadline_slot = proposal
            .execute_after_slot
            .checked_add(OPERATOR_EXECUTION_WINDOW_SLOTS)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        proposal.rent_refund = ctx.accounts.payer.key();
        proposal.executed = false;
        proposal.bump = ctx.bumps.proposal;
        proposal.digest = proposal.intent_digest()?;
        // Replacing a bundle invalidates the previous one and restarts all notice.
        market.operator.pending = proposal.key();
        market.operator.next_nonce = market
            .operator
            .next_nonce
            .checked_add(1)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        emit_cpi!(OperatorChangeAnnounced {
            market: market.key(),
            proposal: proposal.key(),
            operator: proposal.operator,
            nonce: proposal.nonce,
            digest: proposal.digest,
            execute_after_slot: proposal.execute_after_slot,
            deadline_slot: proposal.deadline_slot,
            epoch: proposal.epoch,
            policy_version: proposal.policy_version,
            updates: proposal.updates.clone(),
            next_operator: proposal.next_operator,
            revenue_recipient: proposal.revenue_recipient,
            interest_share_bps: proposal.interest_share_bps,
            metadata: proposal.metadata.clone(),
            family_revisions: proposal.family_revisions
        });
        Ok(())
    }
}

#[event_cpi]
#[derive(Accounts)]
pub struct ExecuteOperatorChange<'info> {
    #[account(mut, seeds=[MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump=market.bump)]
    pub market: Box<Account<'info, Market>>,
    #[account(seeds=[crate::constants::FUTARCHY_AUTHORITY_SEED_PREFIX],bump=futarchy_authority.bump)]
    pub futarchy_authority: Box<Account<'info, crate::state::FutarchyAuthority>>,

    #[account(mut, has_one=market, seeds=[OPERATOR_PROPOSAL_SEED, market.key().as_ref(), &proposal.nonce.to_le_bytes()], bump=proposal.bump)]
    pub proposal: Box<Account<'info, OperatorProposal>>,
    pub accepting_operator: Option<Signer<'info>>,
    #[account(mut)]
    pub previous_revenue: Option<Box<Account<'info, OperatorRevenueAccount>>>,
}

impl ExecuteOperatorChange<'_> {
    pub fn handle<'info>(ctx: Context<'_, '_, '_, 'info, ExecuteOperatorChange<'info>>) -> Result<()> {
        let clock = Clock::get()?;
        let market = &mut ctx.accounts.market;
        let proposal = &mut ctx.accounts.proposal;
        market.assert_current_version()?;
        proposal.assert_ready(market, proposal.key(), clock.slot)?;
        market.validate_operator_updates(&proposal.updates)?;
        if let Some(next) = proposal.next_operator {
            if next != Pubkey::default() {
                let accepting = ctx
                    .accounts
                    .accepting_operator
                    .as_ref()
                    .ok_or(ErrorCode::InvalidOperator)?;
                require_keys_eq!(accepting.key(), next, ErrorCode::InvalidOperator);
            }
        }
        if let Some(share) = proposal.interest_share_bps {
            require!(
                share <= MAX_OPERATOR_INTEREST_SHARE_BPS,
                ErrorCode::OperatorPolicyExceeded
            );
        }
        let recipient = proposal
            .revenue_recipient
            .or(proposal.next_operator.filter(|key| *key != Pubkey::default()))
            .unwrap_or(market.operator.revenue_recipient);
        require!(recipient != Pubkey::default(), ErrorCode::InvalidRecipient);
        crate::instructions::accounting::accrue_market_interest(
            market,
            clock.slot,
            ctx.accounts.event_authority.to_account_info(),
            ctx.accounts.futarchy_authority.revenue_share.interest_bps,
        )?;
        if !proposal.updates.is_empty() {
            market.execute_parameter_updates_authenticated(&proposal.updates, &clock, ctx.remaining_accounts)?;
        }
        if proposal
            .interest_share_bps
            .is_some_and(|share| share > market.operator.interest_share_bps)
        {
            market.bind_oracle(ctx.remaining_accounts, &clock)?;
            market.refresh_risk_at_slot(clock.slot)?;
            market.assert_parameter_execution_utilization()?;
            market.assert_market_health()?;
        }
        if recipient != market.operator.revenue_recipient {
            let archive = ctx
                .accounts
                .previous_revenue
                .as_mut()
                .ok_or(ErrorCode::OperatorRevenueArchiveRequired)?;
            let key = Pubkey::create_program_address(
                &[
                    OPERATOR_REVENUE_SEED,
                    market.key().as_ref(),
                    &market.operator.revenue_epoch.to_le_bytes(),
                    &[archive.bump],
                ],
                &crate::ID,
            )
            .map_err(|_| error!(ErrorCode::InvalidArgument))?;
            require_keys_eq!(archive.key(), key, ErrorCode::InvalidArgument);
            require_keys_eq!(archive.market, market.key(), ErrorCode::InvalidMarket);
            require!(
                archive.revenue_epoch == market.operator.revenue_epoch && !archive.sealed,
                ErrorCode::InvalidArgument
            );
            require_keys_eq!(
                archive.recipient,
                market.operator.revenue_recipient,
                ErrorCode::InvalidRecipient
            );
            for i in 0..4 {
                archive.claimable[i] = market.income.get_mut()?.revenue[i].take_operator_cash()?;
            }
            archive.sealed = true;
            market.operator.revenue_epoch = market
                .operator
                .revenue_epoch
                .checked_add(1)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            market.operator.revenue_recipient = recipient;
        } else {
            require!(ctx.accounts.previous_revenue.is_none(), ErrorCode::InvalidArgument);
        }
        if let Some(share) = proposal.interest_share_bps {
            market.operator.interest_share_bps = share;
        }
        if let Some(next) = proposal.next_operator {
            market.operator.authority = next;
            market.operator.epoch = market
                .operator
                .epoch
                .checked_add(1)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            emit_cpi!(OperatorChanged {
                market: market.key(),
                operator: next,
                epoch: market.operator.epoch
            });
        }
        market.operator.pending = Pubkey::default();
        proposal.executed = true;
        emit_cpi!(OperatorChangeResolved {
            market: market.key(),
            proposal: proposal.key(),
            executed: true
        });
        Ok(())
    }
}

#[event_cpi]
#[derive(Accounts)]
pub struct ResolveOperatorChange<'info> {
    #[account(mut, seeds=[MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump=market.bump)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut,close=rent_refund,has_one=market,has_one=rent_refund,seeds=[OPERATOR_PROPOSAL_SEED,market.key().as_ref(),&proposal.nonce.to_le_bytes()],bump=proposal.bump)]
    pub proposal: Box<Account<'info, OperatorProposal>>,
    /// CHECK: Original proposal payer, checked by has_one; receives only its account rent.
    #[account(mut)]
    pub rent_refund: UncheckedAccount<'info>,
    pub operator: Option<Signer<'info>>,
}
impl ResolveOperatorChange<'_> {
    pub fn handle(ctx: Context<Self>, cancel: bool) -> Result<()> {
        let market = &mut ctx.accounts.market;
        let proposal = &ctx.accounts.proposal;
        let current = proposal.current(market, proposal.key());
        if cancel {
            market
                .operator
                .assert_authority(ctx.accounts.operator.as_ref().ok_or(ErrorCode::InvalidOperator)?.key())?;
        }
        require!(
            cancel || !current || Clock::get()?.slot > proposal.deadline_slot,
            ErrorCode::OperatorChangePending
        );
        if market.operator.pending == proposal.key() {
            market.operator.pending = Pubkey::default();
        }
        emit_cpi!(OperatorChangeResolved {
            market: market.key(),
            proposal: proposal.key(),
            executed: proposal.executed
        });
        Ok(())
    }
}
