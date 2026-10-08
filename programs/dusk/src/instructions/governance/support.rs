use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};

use crate::{
    account::get_size_with_discriminator,
    constants::*,
    errors::ErrorCode,
    events::{ParameterProposalQueued, ParameterProposalSupportWithdrawn, ParameterProposalSupported},
    generate_market_seeds,
    instructions::accounts::validate_lp_mint,
    state::{
        Market, ParameterProposal, ParameterProposalStatus, ParameterProposalTombstone, ProposalSupport, YieldAccount,
        YieldTokenKind,
    },
    token::{token_burn, token_mint_to},
};

use super::{
    carry_forward_governance_yield, checkpoint_supporter_yield, direct_ylp_eligible_supply,
    validate_governance_token_accounts, validate_market_pda, validate_supporter_accounts,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SupportParameterProposalArgs {
    pub amount: u64,
    /// Digest of the proposal the supporter reviewed. A proposal never
    /// changes and its address never holds another one (it ends as a
    /// tombstone), so this is defence in depth: a support names exactly the
    /// updates and metadata its sender saw.
    pub digest: [u8; 32],
}

#[event_cpi]
#[derive(Accounts)]
pub struct SupportParameterProposal<'info> {
    #[account(mut)]
    pub supporter: Signer<'info>,

    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(mut)]
    pub proposal: Box<Account<'info, ParameterProposal>>,

    #[account(
        init_if_needed,
        payer = supporter,
        space = get_size_with_discriminator::<ProposalSupport>(),
        seeds = [
            PROPOSAL_SUPPORT_SEED_PREFIX,
            proposal.key().as_ref(),
            supporter.key().as_ref(),
        ],
        bump,
    )]
    pub proposal_support: Box<Account<'info, ProposalSupport>>,

    #[account(mut, address = market.ylp_mint)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub supporter_ylp_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            supporter.key().as_ref(),
            ylp_mint.key().as_ref(),
            market.base_side.asset_mint.as_ref(),
            &[YieldTokenKind::Ylp.code()],
        ],
        bump = base_yield_account.bump,
    )]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,

    #[account(
        mut,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            supporter.key().as_ref(),
            ylp_mint.key().as_ref(),
            market.quote_side.asset_mint.as_ref(),
            &[YieldTokenKind::Ylp.code()],
        ],
        bump = quote_yield_account.bump,
    )]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,

    pub base_hlp_ylp_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub quote_hlp_ylp_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl<'info> SupportParameterProposal<'info> {
    pub fn validate(&self, args: &SupportParameterProposalArgs) -> Result<u64> {
        require!(args.amount > 0, ErrorCode::AmountZero);
        require_gte!(
            self.supporter_ylp_account.amount,
            args.amount,
            ErrorCode::InsufficientBalance
        );
        self.proposal.assert_account(self.market.key(), self.proposal.key())?;
        require!(self.proposal.digest == args.digest, ErrorCode::ProposalDigestMismatch);
        require!(
            self.proposal.status == ParameterProposalStatus::Collecting,
            ErrorCode::ProposalNotCollecting
        );
        require!(
            self.proposal.revisions_current(&self.market.parameter_revisions),
            ErrorCode::ProposalStale
        );
        validate_governance_token_accounts(
            &self.market,
            &self.ylp_mint,
            &self.base_hlp_ylp_vault,
            &self.quote_hlp_ylp_vault,
        )?;
        validate_supporter_accounts(
            &self.market,
            self.supporter.key(),
            &self.ylp_mint,
            &self.supporter_ylp_account,
            &self.base_yield_account,
            &self.quote_yield_account,
        )?;
        direct_ylp_eligible_supply(
            &self.market,
            self.ylp_mint.supply,
            self.base_hlp_ylp_vault.amount,
            self.quote_hlp_ylp_vault.amount,
        )
    }

    pub fn handle_support(ctx: Context<'_, '_, '_, 'info, Self>, args: SupportParameterProposalArgs) -> Result<()> {
        let eligible_supply = ctx.accounts.validate(&args)?;
        let clock = Clock::get()?;
        let proposal_key = ctx.accounts.proposal.key();
        let supporter_key = ctx.accounts.supporter.key();
        crate::instructions::accounting::accrue_market_interest(
            &mut ctx.accounts.market,
            clock.slot,
            ctx.accounts.event_authority.to_account_info(),
        )?;
        let indexes = carry_forward_governance_yield(&mut ctx.accounts.market, clock.slot)?;

        if ctx.accounts.proposal_support.proposal == Pubkey::default() {
            ctx.accounts.proposal_support.initialize(
                proposal_key,
                supporter_key,
                indexes.base_swap_fee_q64,
                indexes.base_interest_q64,
                indexes.quote_swap_fee_q64,
                indexes.quote_interest_q64,
                ctx.bumps.proposal_support,
            );
        }
        ctx.accounts
            .proposal_support
            .assert_account(proposal_key, supporter_key, ctx.bumps.proposal_support)?;

        checkpoint_supporter_yield(
            &mut ctx.accounts.base_yield_account,
            &mut ctx.accounts.quote_yield_account,
            ctx.accounts.supporter_ylp_account.amount,
            indexes,
        )?;
        ctx.accounts.proposal_support.accrue_virtual_yield(
            indexes.base_swap_fee_q64,
            indexes.base_interest_q64,
            indexes.quote_swap_fee_q64,
            indexes.quote_interest_q64,
        )?;
        token_burn(
            ctx.accounts.supporter.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            ctx.accounts.ylp_mint.to_account_info(),
            ctx.accounts.supporter_ylp_account.to_account_info(),
            args.amount,
            &[],
        )?;

        ctx.accounts.proposal_support.locked_amount = ctx
            .accounts
            .proposal_support
            .locked_amount
            .checked_add(args.amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        ctx.accounts.proposal.total_locked = ctx
            .accounts
            .proposal
            .total_locked
            .checked_add(args.amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        ctx.accounts.market.governance_locked_ylp = ctx
            .accounts
            .market
            .governance_locked_ylp
            .checked_add(args.amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let queued = ctx
            .accounts
            .proposal
            .queue_if_supported(eligible_supply, clock.unix_timestamp)?;
        emit_cpi!(ParameterProposalSupported {
            proposal: proposal_key,
            supporter: supporter_key,
            amount: args.amount,
            supporter_locked: ctx.accounts.proposal_support.locked_amount,
            total_locked: ctx.accounts.proposal.total_locked,
            status: ctx.accounts.proposal.status.code(),
        });
        if queued {
            emit_cpi!(ParameterProposalQueued {
                proposal: proposal_key,
                total_locked: ctx.accounts.proposal.queued_support,
                eligible_supply: ctx.accounts.proposal.queued_eligible_ylp,
                queued_at: ctx.accounts.proposal.queued_at,
                execute_after: ctx.accounts.proposal.execute_after,
                execution_deadline: ctx.accounts.proposal.execution_deadline,
            });
        }
        Ok(())
    }
}

#[event_cpi]
#[derive(Accounts)]
pub struct WithdrawParameterSupport<'info> {
    #[account(mut)]
    pub supporter: Signer<'info>,

    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, Market>>,

    /// CHECK: The parameter proposal, opened and saved by the handler rather
    /// than by Anchor: when its last supporter withdraws it shrinks to a
    /// `ParameterProposalTombstone`, which a typed account would overwrite with
    /// the full proposal on exit. `validate` checks its owner, discriminator,
    /// address and digest.
    #[account(mut)]
    pub proposal: UncheckedAccount<'info>,

    /// CHECK: The proposer who paid the proposal's rent, checked against the
    /// proposal in `validate`. Only receives lamports: the rent above the
    /// tombstone's own when the last supporter withdraws.
    #[account(mut)]
    pub proposer: UncheckedAccount<'info>,

    #[account(
        mut,
        close = supporter,
        seeds = [
            PROPOSAL_SUPPORT_SEED_PREFIX,
            proposal.key().as_ref(),
            supporter.key().as_ref(),
        ],
        bump = proposal_support.bump,
    )]
    pub proposal_support: Box<Account<'info, ProposalSupport>>,

    #[account(mut, address = market.ylp_mint)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub supporter_ylp_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            supporter.key().as_ref(),
            ylp_mint.key().as_ref(),
            market.base_side.asset_mint.as_ref(),
            &[YieldTokenKind::Ylp.code()],
        ],
        bump = base_yield_account.bump,
    )]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,

    #[account(
        mut,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            supporter.key().as_ref(),
            ylp_mint.key().as_ref(),
            market.quote_side.asset_mint.as_ref(),
            &[YieldTokenKind::Ylp.code()],
        ],
        bump = quote_yield_account.bump,
    )]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,

    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> WithdrawParameterSupport<'info> {
    pub fn validate(&self) -> Result<ParameterProposal> {
        validate_market_pda(&self.market, self.market.key())?;
        require_keys_eq!(self.market.ylp_mint, self.ylp_mint.key(), ErrorCode::InvalidLpMintKey);
        validate_lp_mint(&self.ylp_mint, self.market.key(), self.market.base_side.asset_decimals)?;
        require_keys_eq!(*self.proposal.owner, crate::ID, ErrorCode::InvalidParameterProposal);
        let proposal = ParameterProposal::try_deserialize(&mut &self.proposal.try_borrow_data()?[..])?;
        proposal.assert_account(self.market.key(), self.proposal.key())?;
        require_keys_eq!(
            self.proposer.key(),
            proposal.proposer,
            ErrorCode::InvalidParameterProposal
        );
        self.proposal_support
            .assert_account(self.proposal.key(), self.supporter.key(), self.proposal_support.bump)?;
        require!(
            self.proposal_support.locked_amount > 0,
            ErrorCode::InvalidProposalSupport
        );
        validate_supporter_accounts(
            &self.market,
            self.supporter.key(),
            &self.ylp_mint,
            &self.supporter_ylp_account,
            &self.base_yield_account,
            &self.quote_yield_account,
        )?;
        Ok(proposal)
    }

    pub fn handle_withdraw(ctx: Context<'_, '_, '_, 'info, Self>) -> Result<()> {
        let mut proposal = ctx.accounts.validate()?;
        let clock = Clock::get()?;
        proposal.mark_stale_if_revision_changed(&ctx.accounts.market.parameter_revisions);
        proposal.mark_expired_if_past_deadline(clock.unix_timestamp);
        require!(
            proposal.status != ParameterProposalStatus::Queued,
            ErrorCode::ProposalSupportFrozen
        );

        let amount = ctx.accounts.proposal_support.locked_amount;
        crate::instructions::accounting::accrue_market_interest(
            &mut ctx.accounts.market,
            clock.slot,
            ctx.accounts.event_authority.to_account_info(),
        )?;
        let indexes = carry_forward_governance_yield(&mut ctx.accounts.market, clock.slot)?;
        checkpoint_supporter_yield(
            &mut ctx.accounts.base_yield_account,
            &mut ctx.accounts.quote_yield_account,
            ctx.accounts.supporter_ylp_account.amount,
            indexes,
        )?;
        ctx.accounts.proposal_support.accrue_virtual_yield(
            indexes.base_swap_fee_q64,
            indexes.base_interest_q64,
            indexes.quote_swap_fee_q64,
            indexes.quote_interest_q64,
        )?;
        ctx.accounts
            .proposal_support
            .base_yield
            .merge_into(&mut ctx.accounts.base_yield_account)?;
        ctx.accounts
            .proposal_support
            .quote_yield
            .merge_into(&mut ctx.accounts.quote_yield_account)?;

        proposal.total_locked = proposal
            .total_locked
            .checked_sub(amount)
            .ok_or(ErrorCode::InvalidProposalSupport)?;
        ctx.accounts.market.governance_locked_ylp = ctx
            .accounts
            .market
            .governance_locked_ylp
            .checked_sub(amount)
            .ok_or(ErrorCode::InvalidProposalSupport)?;
        proposal.cancel_if_below_sponsorship_floor();

        let market_seeds = generate_market_seeds!(ctx.accounts.market);
        token_mint_to(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            ctx.accounts.ylp_mint.to_account_info(),
            ctx.accounts.supporter_ylp_account.to_account_info(),
            amount,
            &[&market_seeds[..]],
        )?;
        // Support only reaches zero once the proposal has ended: a collecting
        // proposal below its sponsorship floor was cancelled above, and queued
        // support cannot be withdrawn. Nothing reads the proposal after its
        // last supporter leaves, so it becomes a tombstone: the discriminator
        // alone, kept rent-exempt so the address can never hold another
        // proposal, with the rest of its rent returned to the proposer.
        let proposal_tombstoned = proposal.total_locked == 0;
        emit_cpi!(ParameterProposalSupportWithdrawn {
            proposal: ctx.accounts.proposal.key(),
            supporter: ctx.accounts.supporter.key(),
            amount,
            total_locked: proposal.total_locked,
            status: proposal.status.code(),
            proposal_tombstoned,
        });
        let proposal_info = ctx.accounts.proposal.to_account_info();
        if proposal_tombstoned {
            let space = get_size_with_discriminator::<ParameterProposalTombstone>();
            let refund = proposal_info
                .lamports()
                .checked_sub(Rent::get()?.minimum_balance(space))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            proposal_info.realloc(space, false)?;
            proposal_info
                .try_borrow_mut_data()?
                .copy_from_slice(ParameterProposalTombstone::DISCRIMINATOR);
            proposal_info.sub_lamports(refund)?;
            ctx.accounts.proposer.add_lamports(refund)?;
        } else {
            let mut data = proposal_info.try_borrow_mut_data()?;
            let mut writer: &mut [u8] = &mut data;
            proposal.try_serialize(&mut writer)?;
        }
        Ok(())
    }
}
