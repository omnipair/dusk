use crate::{
    account::get_size_with_discriminator,
    errors::ErrorCode,
    instructions::market::{validate_lp_metadata_args, InitializeLpMetadataArgs},
    state::{JuniorCohort, Market, TrancheIncome},
};
use anchor_lang::prelude::*;
use anchor_spl::metadata::Metadata;
use anchor_spl::token_interface::{Mint, Token2022};

#[event]
pub struct JuniorCohortInitialized {
    pub market: Pubkey,
    pub mint: Pubkey,
    pub generation: u64,
    pub previous_mint: Pubkey,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeJuniorArgs {
    pub generation: u64,
    pub metadata: InitializeLpMetadataArgs,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: InitializeJuniorArgs)]
pub struct InitializeJunior<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion)]
    pub market: Box<Account<'info, Market>>,
    #[account(init, payer = payer, seeds = [b"junior_mint", market.key().as_ref(), &args.generation.to_le_bytes()], bump,
        mint::decimals = market.base_side.asset_decimals, mint::authority = market,
        mint::token_program = token_2022_program, extensions::transfer_hook::program_id = crate::ID)]
    pub junior_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(init, payer = payer, space = get_size_with_discriminator::<JuniorCohort>(),
        seeds = [b"junior_cohort", market.key().as_ref(), junior_mint.key().as_ref()], bump)]
    pub cohort: Box<Account<'info, JuniorCohort>>,
    #[account(mut)]
    pub previous_cohort: Option<Box<Account<'info, JuniorCohort>>>,
    /// CHECK: The metadata program validates the canonical mint metadata PDA.
    #[account(mut)]
    pub token_metadata: UncheckedAccount<'info>,
    /// CHECK: Fixed instructions sysvar, validated by the metadata program.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub sysvar_instructions: UncheckedAccount<'info>,
    pub token_metadata_program: Program<'info, Metadata>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl InitializeJunior<'_> {
    pub fn handle_initialize(ctx: Context<Self>, args: InitializeJuniorArgs) -> Result<()> {
        validate_lp_metadata_args(&args.metadata)?;
        let market = &mut ctx.accounts.market;
        market.assert_current_version()?;
        let previous_mint = market.junior_mint;
        if market.junior_mint != Pubkey::default() {
            require_eq!(market.tranches.junior_reserve_shares, 0, ErrorCode::InvalidArgument);
            let old = ctx
                .accounts
                .previous_cohort
                .as_mut()
                .ok_or(ErrorCode::InvalidArgument)?;
            require_keys_eq!(old.market, market.key(), ErrorCode::InvalidMarket);
            require_keys_eq!(old.mint, market.junior_mint, ErrorCode::InvalidLpMintKey);
            require!(!old.retired, ErrorCode::InvalidArgument);
            require_eq!(old.generation, market.tranches.generation, ErrorCode::InvalidArgument);
            let expected = Pubkey::find_program_address(
                &[b"junior_cohort", market.key().as_ref(), market.junior_mint.as_ref()],
                &crate::ID,
            )
            .0;
            require_keys_eq!(old.key(), expected, ErrorCode::InvalidArgument);
            market.checkpoint_junior_seed()?;
            old.final_income = [market.base_side.fees.junior, market.quote_side.fees.junior];
            old.retired = true;
            old.income_end = market.income.get()?.junior_end;
            market.income.get_mut()?.junior_end = Default::default();
            market.tranches.retire_wiped_generation()?;
            // Active reconciliation tracks only receipts of the replacement mint.
            // Retired receipt owners retain their original mint and income claims.

            let income_share_bps = market.tranche_policy.junior_income_share_bps;
            for asset in [crate::state::MarketAsset::Base, crate::state::MarketAsset::Quote] {
                let side = market.side_mut(asset);
                let previous = side.fees.junior;
                side.fees.junior = TrancheIncome {
                    initialized: true,
                    income_share_bps,
                    seed_swap_amount: previous.seed_swap_amount,
                    seed_interest_amount: previous.seed_interest_amount,
                    carry: previous.carry,
                    ..Default::default()
                };
            }
            market.synchronize_tranche_income();
        } else {
            require!(ctx.accounts.previous_cohort.is_none(), ErrorCode::InvalidArgument);
            let income_share_bps = market.tranche_policy.junior_income_share_bps;
            market.base_side.fees.junior.income_share_bps = income_share_bps;
            market.quote_side.fees.junior.income_share_bps = income_share_bps;
        }
        require_eq!(args.generation, market.tranches.generation, ErrorCode::InvalidArgument);
        market.junior_mint = ctx.accounts.junior_mint.key();
        let cohort = &mut ctx.accounts.cohort;
        cohort.market = market.key();
        cohort.mint = market.junior_mint;
        cohort.generation = args.generation;
        cohort.bump = ctx.bumps.cohort;
        crate::instructions::market::create_lp_metadata(
            market,
            &ctx.accounts.payer,
            &ctx.accounts.junior_mint,
            &ctx.accounts.token_metadata,
            &ctx.accounts.system_program,
            &ctx.accounts.sysvar_instructions,
            &ctx.accounts.token_2022_program,
            &ctx.accounts.token_metadata_program,
            args.metadata,
        )?;
        emit_cpi!(JuniorCohortInitialized {
            market: market.key(),
            mint: market.junior_mint,
            generation: args.generation,
            previous_mint,
        });
        Ok(())
    }
}
