use anchor_lang::prelude::*;
use anchor_spl::{
    metadata::{
        mpl_token_metadata::types::DataV2, update_metadata_accounts_v2, Metadata, MetadataAccount,
        UpdateMetadataAccountsV2,
    },
    token_interface::Mint,
};

use crate::{constants::MARKET_V2_SEED_PREFIX, errors::ErrorCode, generate_market_seeds, state::Market};

use super::{validate_lp_metadata_args, InitializeLpMetadataArgs};

#[derive(Accounts)]
pub struct UpdateLpMetadata<'info> {
    pub operator: Signer<'info>,
    #[account(
        constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion,
        seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()],
        bump = market.bump,
        constraint = market.operator.authority == operator.key() @ ErrorCode::InvalidOperator
    )]
    pub market: Box<Account<'info, Market>>,
    #[account(constraint = [market.srlp_mint, market.base_side.hlp_mint, market.quote_side.hlp_mint, market.junior_mint].contains(&lp_mint.key()) @ ErrorCode::InvalidLpMintKey)]
    pub lp_mint: InterfaceAccount<'info, Mint>,
    #[account(
        mut,
        seeds = [b"metadata", token_metadata_program.key().as_ref(), lp_mint.key().as_ref()],
        bump,
        seeds::program = token_metadata_program.key(),
        constraint = token_metadata.mint == lp_mint.key() @ ErrorCode::InvalidLpMintKey,
        constraint = token_metadata.update_authority == market.key() @ ErrorCode::InvalidOperator
    )]
    pub token_metadata: Account<'info, MetadataAccount>,
    pub token_metadata_program: Program<'info, Metadata>,
}

impl<'info> UpdateLpMetadata<'info> {
    pub fn handle(ctx: Context<Self>, args: InitializeLpMetadataArgs) -> Result<()> {
        let market = &ctx.accounts.market;
        market.assert_current_version()?;
        validate_lp_metadata_args(&args)?;
        let existing = &ctx.accounts.token_metadata;
        // Cosmetic edits cannot change authority, royalties, creators,
        // collections, uses, mutability, or any market/mint accounting.
        update_metadata_accounts_v2(
            CpiContext::new_with_signer(
                ctx.accounts.token_metadata_program.to_account_info(),
                UpdateMetadataAccountsV2 {
                    metadata: existing.to_account_info(),
                    update_authority: market.to_account_info(),
                },
                &[&generate_market_seeds!(market)[..]],
            ),
            None,
            Some(DataV2 {
                name: args.name,
                symbol: args.symbol,
                uri: args.uri,
                seller_fee_basis_points: existing.seller_fee_basis_points,
                creators: existing.creators.clone(),
                collection: existing.collection.clone(),
                uses: existing.uses.clone(),
            }),
            None,
            None,
        )
    }
}
