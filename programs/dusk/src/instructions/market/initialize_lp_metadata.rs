use anchor_lang::{prelude::*, solana_program::sysvar::instructions as sysvar_instructions};
use anchor_spl::{
    metadata::{
        mpl_token_metadata::{
            instructions::{CreateV1Cpi, CreateV1CpiAccounts, CreateV1InstructionArgs},
            types::TokenStandard,
            ID as MPL_TOKEN_METADATA_PROGRAM_ID,
        },
        Metadata,
    },
    token_interface::{Mint, Token2022},
};

use crate::{
    constants::*, errors::ErrorCode, generate_market_seeds, instructions::common::validate_lp_mint, state::Market,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeLpMetadataArgs {
    pub name: String,
    pub symbol: String,
    pub uri: String,
}

#[derive(Accounts)]
pub struct InitializeLpMetadata<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    pub market: Box<Account<'info, Market>>,

    #[account(mut)]
    pub lp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        seeds = [
            METADATA_SEED_PREFIX,
            MPL_TOKEN_METADATA_PROGRAM_ID.as_ref(),
            lp_mint.key().as_ref(),
        ],
        seeds::program = MPL_TOKEN_METADATA_PROGRAM_ID,
        bump
    )]
    /// CHECK: derived/checked via seeds above.
    pub lp_token_metadata: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,

    #[account(address = sysvar_instructions::ID)]
    /// CHECK: the Metaplex create_v1 CPI requires the instructions sysvar.
    pub sysvar_instructions: UncheckedAccount<'info>,

    pub token_2022_program: Program<'info, Token2022>,

    pub token_metadata_program: Program<'info, Metadata>,
}

impl<'info> InitializeLpMetadata<'info> {
    pub fn validate(&self, args: &InitializeLpMetadataArgs) -> Result<()> {
        require!(args.name.len() <= 32, ErrorCode::InvalidLpName);
        require!(args.name.is_ascii(), ErrorCode::InvalidLpName);
        require!(args.symbol.len() <= 10, ErrorCode::InvalidLpSymbol);
        require!(args.symbol.is_ascii(), ErrorCode::InvalidLpSymbol);
        require!(args.uri.len() <= 200, ErrorCode::InvalidLpUri);
        require!(args.uri.starts_with("http"), ErrorCode::InvalidLpUri);

        let lp_mint = self.lp_mint.key();
        let (decimals, vanity_suffix) = if lp_mint == self.market.ylp_mint {
            (self.market.base_side.asset_decimals, "yLP")
        } else if lp_mint == self.market.base_side.hlp_mint {
            (self.market.base_side.asset_decimals, "hLP")
        } else if lp_mint == self.market.quote_side.hlp_mint {
            (self.market.quote_side.asset_decimals, "hLP")
        } else {
            return err!(ErrorCode::InvalidLpMintKey);
        };
        validate_lp_mint(&self.lp_mint, self.market.key(), decimals)?;
        #[cfg(feature = "production")]
        {
            let mint_key = lp_mint.to_string();
            let start_idx = mint_key
                .len()
                .checked_sub(vanity_suffix.len())
                .ok_or(ErrorCode::InvalidLpMintKey)?;
            require_eq!(vanity_suffix, &mint_key[start_idx..], ErrorCode::InvalidLpMintKey);
        }
        #[cfg(not(feature = "production"))]
        let _ = vanity_suffix;
        Ok(())
    }

    pub fn handle_initialize(ctx: Context<Self>, args: InitializeLpMetadataArgs) -> Result<()> {
        let token_metadata_program = ctx.accounts.token_metadata_program.to_account_info();
        let metadata = ctx.accounts.lp_token_metadata.to_account_info();
        let mint = ctx.accounts.lp_mint.to_account_info();
        let authority = ctx.accounts.market.to_account_info();
        let payer = ctx.accounts.payer.to_account_info();
        let system_program = ctx.accounts.system_program.to_account_info();
        let instructions_sysvar = ctx.accounts.sysvar_instructions.to_account_info();
        let token_2022_program = ctx.accounts.token_2022_program.to_account_info();
        let cpi_accounts = CreateV1CpiAccounts {
            metadata: &metadata,
            master_edition: None,
            mint: (&mint, false),
            authority: &authority,
            payer: &payer,
            update_authority: (&authority, true),
            system_program: &system_program,
            sysvar_instructions: &instructions_sysvar,
            spl_token_program: Some(&token_2022_program),
        };
        let cpi_args = CreateV1InstructionArgs {
            name: args.name,
            symbol: args.symbol,
            uri: args.uri,
            seller_fee_basis_points: 0,
            creators: None,
            primary_sale_happened: false,
            is_mutable: true,
            token_standard: TokenStandard::Fungible,
            collection: None,
            uses: None,
            collection_details: None,
            rule_set: None,
            decimals: None,
            print_supply: None,
        };

        CreateV1Cpi::new(&token_metadata_program, cpi_accounts, cpi_args)
            .invoke_signed(&[&generate_market_seeds!(ctx.accounts.market)[..]])
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/instructions/market/initialize_lp_metadata.rs");
}
