use anchor_lang::prelude::*;
use anchor_spl::{
    metadata::{mpl_token_metadata::ID as MPL_TOKEN_METADATA_PROGRAM_ID, Metadata},
    token_interface::{Mint, Token2022},
};

use crate::{
    constants::{HLP_COHORT_SEED_PREFIX, METADATA_SEED_PREFIX},
    errors::ErrorCode,
    events::HlpCohortRotated,
    instructions::{
        accounts::{derive_hlp_ylp_vault_address, validate_lp_mint},
        market::{create_lp_metadata, require_vanity_suffix, validate_lp_metadata_args, InitializeLpMetadataArgs},
    },
    state::{HlpCohort, HlpVault, Market, MarketAsset},
};

use super::reconcile_live_hlp_supply;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct RotateHlpCohortArgs {
    pub target_asset: u8,
    pub metadata: InitializeLpMetadataArgs,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: RotateHlpCohortArgs)]
pub struct RotateHlpCohort<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
    pub old_hlp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub new_hlp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        seeds = [HLP_COHORT_SEED_PREFIX, market.key().as_ref(), old_hlp_mint.key().as_ref()],
        bump = old_cohort.bump
    )]
    pub old_cohort: Box<Account<'info, HlpCohort>>,
    #[account(
        mut,
        seeds = [METADATA_SEED_PREFIX, MPL_TOKEN_METADATA_PROGRAM_ID.as_ref(), new_hlp_mint.key().as_ref()],
        seeds::program = MPL_TOKEN_METADATA_PROGRAM_ID,
        bump
    )]
    /// CHECK: Canonical Metaplex metadata PDA for the new hLP mint.
    pub new_token_metadata: UncheckedAccount<'info>,
    /// CHECK: The Metaplex CPI validates this canonical sysvar account.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub sysvar_instructions: UncheckedAccount<'info>,
    pub token_2022_program: Program<'info, Token2022>,
    pub token_metadata_program: Program<'info, Metadata>,
    pub system_program: Program<'info, System>,
}

impl<'info> RotateHlpCohort<'info> {
    pub fn handle_rotate(ctx: Context<Self>, args: RotateHlpCohortArgs) -> Result<()> {
        let target_asset = MarketAsset::try_from_code(args.target_asset)?;
        let market = &mut ctx.accounts.market;
        market.assert_current_version()?;
        let market_key = market.key();
        let old_mint = ctx.accounts.old_hlp_mint.key();
        let new_mint = ctx.accounts.new_hlp_mint.key();
        require_keys_eq!(
            market.side(target_asset).hlp_mint,
            old_mint,
            ErrorCode::InvalidLpMintKey
        );
        require_keys_neq!(new_mint, old_mint, ErrorCode::InvalidLpMintKey);
        market.asset_for_hlp_mint(old_mint)?;
        ctx.accounts
            .old_cohort
            .assert_identity(market_key, old_mint, target_asset)?;
        require!(!ctx.accounts.old_cohort.retired, ErrorCode::InvalidArgument);
        require!(ctx.accounts.old_hlp_mint.supply > 0, ErrorCode::InvalidHlpMintSupply);
        require_eq!(ctx.accounts.new_hlp_mint.supply, 0, ErrorCode::NonZeroSupply);
        let decimals = market.side(target_asset).asset_decimals;
        validate_lp_mint(&ctx.accounts.new_hlp_mint, market_key, decimals)?;
        require_vanity_suffix(&ctx.accounts.new_hlp_mint, "hLP")?;
        validate_lp_metadata_args(&args.metadata)?;
        match target_asset {
            MarketAsset::Base => Market::validate_mint_domain(
                market.base_side.asset_mint,
                market.quote_side.asset_mint,
                market.ylp_mint,
                new_mint,
                market.quote_side.hlp_mint,
            )?,
            MarketAsset::Quote => Market::validate_mint_domain(
                market.base_side.asset_mint,
                market.quote_side.asset_mint,
                market.ylp_mint,
                market.base_side.hlp_mint,
                new_mint,
            )?,
        }
        reconcile_live_hlp_supply(market, target_asset, ctx.accounts.old_hlp_mint.supply)?;
        require!(
            market.hlp_terminally_closed(target_asset),
            ErrorCode::HlpSettlementUnavailable
        );
        let retired_vault = match target_asset {
            MarketAsset::Base => market.base_hlp_vault,
            MarketAsset::Quote => market.quote_hlp_vault,
        };
        require!(
            retired_vault.flash_collateral_debit == 0,
            ErrorCode::LiquidationSessionActive
        );
        let new_ylp_vault = derive_hlp_ylp_vault_address(market_key, new_mint, market.ylp_mint).0;

        ctx.accounts.old_cohort.final_vault = retired_vault;
        ctx.accounts.old_cohort.retired = true;
        let fresh_vault = HlpVault {
            ylp_vault: new_ylp_vault,
            ..HlpVault::default()
        };
        match target_asset {
            MarketAsset::Base => market.base_hlp_vault = fresh_vault,
            MarketAsset::Quote => market.quote_hlp_vault = fresh_vault,
        }
        market.side_mut(target_asset).hlp_mint = new_mint;

        create_lp_metadata(
            market,
            &ctx.accounts.payer,
            &ctx.accounts.new_hlp_mint,
            &ctx.accounts.new_token_metadata,
            &ctx.accounts.system_program,
            &ctx.accounts.sysvar_instructions,
            &ctx.accounts.token_2022_program,
            &ctx.accounts.token_metadata_program,
            args.metadata,
        )?;
        emit_cpi!(HlpCohortRotated {
            market: market_key,
            target_asset: args.target_asset,
            retired_mint: old_mint,
            new_mint,
            retired_supply: retired_vault.hlp_supply,
        });
        Ok(())
    }
}
