use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable::UpgradeableLoaderState;
use bincode::Options;

use crate::{
    constants::{
        BPS_DENOMINATOR, FUTARCHY_AUTHORITY_SEED_PREFIX, MAX_REFERRAL_INTEREST_SHARE_BPS,
        REDUCE_ONLY_EMERGENCY_AUTHORITY,
    },
    errors::ErrorCode,
    shared::account::get_size_with_discriminator,
    state::FutarchyAuthority,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitFutarchyAuthorityArgs {
    pub authority: Pubkey,
    pub swap_bps: u16,
    pub interest_bps: u16,
    pub max_referral_interest_share_bps: u16,
    pub futarchy_treasury: Pubkey,
    pub futarchy_treasury_bps: u16,
    pub buybacks_vault: Pubkey,
    pub buybacks_vault_bps: u16,
    pub team_treasury: Pubkey,
    pub team_treasury_bps: u16,
    pub staking_vault: Pubkey,
    pub fee_auction_accepted_mint: Pubkey,
    pub buyback_auction_accepted_mint: Pubkey,
}

#[derive(Accounts)]
pub struct InitFutarchyAuthority<'info> {
    #[account(mut)]
    pub deployer: Signer<'info>,

    #[account(
        init,
        payer = deployer,
        space = get_size_with_discriminator::<FutarchyAuthority>(),
        seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    /// CHECK: PDA derivation is enforced by seeds and owner is validated below.
    #[account(
        seeds = [crate::ID.as_ref()],
        bump,
        seeds::program = anchor_lang::solana_program::bpf_loader_upgradeable::ID
    )]
    pub program_data: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
}

impl<'info> InitFutarchyAuthority<'info> {
    pub fn handle_init(ctx: Context<Self>, args: InitFutarchyAuthorityArgs) -> Result<()> {
        let InitFutarchyAuthority {
            deployer,
            futarchy_authority,
            program_data,
            ..
        } = ctx.accounts;

        // Validate the deployer against the program's upgrade authority.
        require_keys_eq!(
            *program_data.owner,
            anchor_lang::solana_program::bpf_loader_upgradeable::ID,
            ErrorCode::InvalidDeployer
        );

        let data = program_data.try_borrow_data()?;
        let loader_state: UpgradeableLoaderState = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .allow_trailing_bytes()
            .deserialize(&data)
            .map_err(|_| ErrorCode::InvalidDeployer)?;

        let upgrade_authority = match loader_state {
            UpgradeableLoaderState::ProgramData {
                upgrade_authority_address,
                ..
            } => upgrade_authority_address.ok_or(ErrorCode::InvalidDeployer)?,
            _ => return Err(ErrorCode::InvalidDeployer.into()),
        };
        require_keys_eq!(deployer.key(), upgrade_authority, ErrorCode::InvalidDeployer);

        require_gte!(BPS_DENOMINATOR, args.swap_bps, ErrorCode::InvalidSwapFeeBps);
        require_gte!(BPS_DENOMINATOR, args.interest_bps, ErrorCode::InvalidInterestFeeBps);
        require_gte!(
            MAX_REFERRAL_INTEREST_SHARE_BPS,
            args.max_referral_interest_share_bps,
            ErrorCode::InvalidReferralInterestShareBps
        );

        let total_percentage = args
            .futarchy_treasury_bps
            .checked_add(args.buybacks_vault_bps)
            .ok_or(ErrorCode::FeeMathOverflow)?
            .checked_add(args.team_treasury_bps)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require_eq!(total_percentage, BPS_DENOMINATOR, ErrorCode::InvalidDistribution);

        let authority = FutarchyAuthority::initialize(
            args.authority,
            args.swap_bps,
            args.interest_bps,
            args.max_referral_interest_share_bps,
            args.futarchy_treasury,
            args.buybacks_vault,
            args.team_treasury,
            args.staking_vault,
            args.fee_auction_accepted_mint,
            args.buyback_auction_accepted_mint,
            args.futarchy_treasury_bps,
            args.buybacks_vault_bps,
            args.team_treasury_bps,
            ctx.bumps.futarchy_authority,
        )?;
        futarchy_authority.set_inner(authority);

        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateFutarchyAuthorityArgs {
    pub new_authority: Pubkey,
}

#[derive(Accounts)]
pub struct UpdateFutarchyAuthority<'info> {
    #[account(
        mut,
        address = futarchy_authority.authority @ ErrorCode::InvalidFutarchyAuthority
    )]
    pub authority_signer: Signer<'info>,

    #[account(
        mut,
        seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump = futarchy_authority.bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
}

impl<'info> UpdateFutarchyAuthority<'info> {
    pub fn handle_update(ctx: Context<Self>, args: UpdateFutarchyAuthorityArgs) -> Result<()> {
        let futarchy_authority = &mut ctx.accounts.futarchy_authority;
        futarchy_authority.authority = args.new_authority;

        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SetGlobalReduceOnlyArgs {
    pub reduce_only: bool,
}

#[derive(Accounts)]
pub struct SetGlobalReduceOnly<'info> {
    #[account(
        mut,
        address = REDUCE_ONLY_EMERGENCY_AUTHORITY @ ErrorCode::InvalidReduceOnlyAuthority
    )]
    pub authority_signer: Signer<'info>,

    #[account(
        mut,
        seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump = futarchy_authority.bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
}

impl<'info> SetGlobalReduceOnly<'info> {
    pub fn handle_set_global_reduce_only(ctx: Context<Self>, args: SetGlobalReduceOnlyArgs) -> Result<()> {
        let futarchy_authority = &mut ctx.accounts.futarchy_authority;
        futarchy_authority.global_reduce_only = args.reduce_only;
        msg!("Global reduce-only mode set to: {}", args.reduce_only);

        Ok(())
    }
}
