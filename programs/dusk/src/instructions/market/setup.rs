use anchor_lang::{
    prelude::*,
    solana_program::{program::invoke, system_instruction, sysvar::instructions as sysvar_instructions},
};
use anchor_spl::{
    metadata::{
        mpl_token_metadata::{
            instructions::{CreateV1Cpi, CreateV1CpiAccounts, CreateV1InstructionArgs},
            types::TokenStandard,
            ID as MPL_TOKEN_METADATA_PROGRAM_ID,
        },
        Metadata,
    },
    token::{spl_token, Token, TokenAccount},
    token_interface::{Mint, Token2022},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{MarketCreated, MarketEventMetadata},
    generate_market_seeds,
    shared::{account::get_size_with_discriminator, token::create_token_account},
    state::{FutarchyAuthority, Market, MarketConfig, MarketSide},
};

use crate::instructions::common::{
    derive_hlp_ylp_vault_address, require_supported_asset_mint, token_program_for_mint, validate_lp_mint,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeMarketArgs {
    pub config: MarketConfig,
    pub params_hash: [u8; 32],
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: InitializeMarketArgs)]
pub struct InitializeMarket<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        constraint = quote_mint.key() != base_mint.key() @ ErrorCode::InvalidMint
    )]
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        constraint = ylp_mint.key() != base_mint.key() @ ErrorCode::InvalidLpMintKey,
        constraint = ylp_mint.key() != quote_mint.key() @ ErrorCode::InvalidLpMintKey,
    )]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        constraint = base_hlp_mint.key() != base_mint.key() @ ErrorCode::InvalidLpMintKey,
        constraint = base_hlp_mint.key() != quote_mint.key() @ ErrorCode::InvalidLpMintKey,
        constraint = base_hlp_mint.key() != ylp_mint.key() @ ErrorCode::InvalidLpMintKey,
    )]
    pub base_hlp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        constraint = quote_hlp_mint.key() != base_mint.key() @ ErrorCode::InvalidLpMintKey,
        constraint = quote_hlp_mint.key() != quote_mint.key() @ ErrorCode::InvalidLpMintKey,
        constraint = quote_hlp_mint.key() != ylp_mint.key() @ ErrorCode::InvalidLpMintKey,
        constraint = quote_hlp_mint.key() != base_hlp_mint.key() @ ErrorCode::InvalidLpMintKey,
    )]
    pub quote_hlp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        init,
        payer = payer,
        space = get_size_with_discriminator::<Market>(),
        seeds = [
            MARKET_V2_SEED_PREFIX,
            base_mint.key().as_ref(),
            quote_mint.key().as_ref(),
            args.params_hash.as_ref(),
        ],
        bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(
        seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump = futarchy_authority.bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    /// CHECK: Reserve vault PDA for the base asset.
    #[account(
        mut,
        seeds = [
            MARKET_RESERVE_VAULT_SEED_PREFIX,
            market.key().as_ref(),
            base_mint.key().as_ref(),
        ],
        bump
    )]
    pub base_reserve_vault: UncheckedAccount<'info>,
    /// CHECK: Reserve vault PDA for the quote asset.
    #[account(
        mut,
        seeds = [
            MARKET_RESERVE_VAULT_SEED_PREFIX,
            market.key().as_ref(),
            quote_mint.key().as_ref(),
        ],
        bump
    )]
    pub quote_reserve_vault: UncheckedAccount<'info>,
    /// CHECK: Collateral vault PDA for the base asset.
    #[account(
        mut,
        seeds = [
            MARKET_COLLATERAL_VAULT_SEED_PREFIX,
            market.key().as_ref(),
            base_mint.key().as_ref(),
        ],
        bump
    )]
    pub base_collateral_vault: UncheckedAccount<'info>,
    /// CHECK: Collateral vault PDA for the quote asset.
    #[account(
        mut,
        seeds = [
            MARKET_COLLATERAL_VAULT_SEED_PREFIX,
            market.key().as_ref(),
            quote_mint.key().as_ref(),
        ],
        bump
    )]
    pub quote_collateral_vault: UncheckedAccount<'info>,
    /// CHECK: Junior insurance vault PDA for the base asset.
    #[account(
        mut,
        seeds = [
            INSURANCE_SEED_PREFIX,
            market.key().as_ref(),
            base_mint.key().as_ref(),
        ],
        bump
    )]
    pub base_insurance_vault: UncheckedAccount<'info>,
    /// CHECK: Junior insurance vault PDA for the quote asset.
    #[account(
        mut,
        seeds = [
            INSURANCE_SEED_PREFIX,
            market.key().as_ref(),
            quote_mint.key().as_ref(),
        ],
        bump
    )]
    pub quote_insurance_vault: UncheckedAccount<'info>,
    /// CHECK: Non-compounding interest vault PDA for the base asset.
    #[account(
        mut,
        seeds = [
            MARKET_INTEREST_VAULT_SEED_PREFIX,
            market.key().as_ref(),
            base_mint.key().as_ref(),
        ],
        bump
    )]
    pub base_interest_vault: UncheckedAccount<'info>,
    /// CHECK: Non-compounding interest vault PDA for the quote asset.
    #[account(
        mut,
        seeds = [
            MARKET_INTEREST_VAULT_SEED_PREFIX,
            market.key().as_ref(),
            quote_mint.key().as_ref(),
        ],
        bump
    )]
    pub quote_interest_vault: UncheckedAccount<'info>,

    /// CHECK: Validated against futarchy_authority.recipients.team_treasury.
    #[account(address = futarchy_authority.recipients.team_treasury @ ErrorCode::InvalidRecipient)]
    pub team_treasury: AccountInfo<'info>,

    #[account(
        mut,
        constraint = team_treasury_wsol_account.mint == spl_token::native_mint::id(),
        constraint = team_treasury_wsol_account.owner == futarchy_authority.recipients.team_treasury @ ErrorCode::InvalidRecipient,
        constraint = *team_treasury_wsol_account.to_account_info().owner == token_program.key() @ ErrorCode::InvalidTokenProgram,
    )]
    pub team_treasury_wsol_account: Box<Account<'info, TokenAccount>>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> InitializeMarket<'info> {
    pub fn validate(&self, args: &InitializeMarketArgs) -> Result<()> {
        Market::validate_mint_domain(
            self.base_mint.key(),
            self.quote_mint.key(),
            self.ylp_mint.key(),
            self.base_hlp_mint.key(),
            self.quote_hlp_mint.key(),
        )?;
        require_supported_asset_mint(&self.base_mint)?;
        require_supported_asset_mint(&self.quote_mint)?;
        let market = self.market.key();
        validate_lp_mint(&self.ylp_mint, market, self.base_mint.decimals)?;
        validate_lp_mint(&self.base_hlp_mint, market, self.base_mint.decimals)?;
        validate_lp_mint(&self.quote_hlp_mint, market, self.quote_mint.decimals)?;
        require_vanity_suffix(&self.ylp_mint, "yLP")?;
        require_vanity_suffix(&self.base_hlp_mint, "hLP")?;
        require_vanity_suffix(&self.quote_hlp_mint, "hLP")?;
        require!(self.ylp_mint.supply == 0, ErrorCode::NonZeroSupply);
        require!(self.base_hlp_mint.supply == 0, ErrorCode::NonZeroSupply);
        require!(self.quote_hlp_mint.supply == 0, ErrorCode::NonZeroSupply);
        args.config.validate()
    }

    pub fn handle_initialize(ctx: Context<Self>, args: InitializeMarketArgs) -> Result<()> {
        let current_slot = Clock::get()?.slot;
        let market_key = ctx.accounts.market.key();
        let payer_key = ctx.accounts.payer.key();
        let base_mint = ctx.accounts.base_mint.key();
        let quote_mint = ctx.accounts.quote_mint.key();
        let ylp_mint = ctx.accounts.ylp_mint.key();
        let base_hlp_mint = ctx.accounts.base_hlp_mint.key();
        let quote_hlp_mint = ctx.accounts.quote_hlp_mint.key();
        let base_collateral_vault = ctx.accounts.base_collateral_vault.key();
        let quote_collateral_vault = ctx.accounts.quote_collateral_vault.key();
        let base_insurance_vault = ctx.accounts.base_insurance_vault.key();
        let quote_insurance_vault = ctx.accounts.quote_insurance_vault.key();

        // Create reserve, collateral, insurance, and interest vaults for both assets.
        let base_token_program = token_program_for_mint(
            &ctx.accounts.base_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let quote_token_program = token_program_for_mint(
            &ctx.accounts.quote_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        create_vault_token_account(
            &ctx.accounts.market,
            &ctx.accounts.payer,
            &ctx.accounts.base_reserve_vault,
            &ctx.accounts.base_mint,
            &ctx.accounts.system_program,
            &base_token_program,
            MARKET_RESERVE_VAULT_SEED_PREFIX,
            ctx.bumps.base_reserve_vault,
        )?;
        create_vault_token_account(
            &ctx.accounts.market,
            &ctx.accounts.payer,
            &ctx.accounts.quote_reserve_vault,
            &ctx.accounts.quote_mint,
            &ctx.accounts.system_program,
            &quote_token_program,
            MARKET_RESERVE_VAULT_SEED_PREFIX,
            ctx.bumps.quote_reserve_vault,
        )?;
        create_vault_token_account(
            &ctx.accounts.market,
            &ctx.accounts.payer,
            &ctx.accounts.base_collateral_vault,
            &ctx.accounts.base_mint,
            &ctx.accounts.system_program,
            &base_token_program,
            MARKET_COLLATERAL_VAULT_SEED_PREFIX,
            ctx.bumps.base_collateral_vault,
        )?;
        create_vault_token_account(
            &ctx.accounts.market,
            &ctx.accounts.payer,
            &ctx.accounts.quote_collateral_vault,
            &ctx.accounts.quote_mint,
            &ctx.accounts.system_program,
            &quote_token_program,
            MARKET_COLLATERAL_VAULT_SEED_PREFIX,
            ctx.bumps.quote_collateral_vault,
        )?;
        create_vault_token_account(
            &ctx.accounts.market,
            &ctx.accounts.payer,
            &ctx.accounts.base_insurance_vault,
            &ctx.accounts.base_mint,
            &ctx.accounts.system_program,
            &base_token_program,
            INSURANCE_SEED_PREFIX,
            ctx.bumps.base_insurance_vault,
        )?;
        create_vault_token_account(
            &ctx.accounts.market,
            &ctx.accounts.payer,
            &ctx.accounts.quote_insurance_vault,
            &ctx.accounts.quote_mint,
            &ctx.accounts.system_program,
            &quote_token_program,
            INSURANCE_SEED_PREFIX,
            ctx.bumps.quote_insurance_vault,
        )?;
        create_vault_token_account(
            &ctx.accounts.market,
            &ctx.accounts.payer,
            &ctx.accounts.base_interest_vault,
            &ctx.accounts.base_mint,
            &ctx.accounts.system_program,
            &base_token_program,
            MARKET_INTEREST_VAULT_SEED_PREFIX,
            ctx.bumps.base_interest_vault,
        )?;
        create_vault_token_account(
            &ctx.accounts.market,
            &ctx.accounts.payer,
            &ctx.accounts.quote_interest_vault,
            &ctx.accounts.quote_mint,
            &ctx.accounts.system_program,
            &quote_token_program,
            MARKET_INTEREST_VAULT_SEED_PREFIX,
            ctx.bumps.quote_interest_vault,
        )?;

        // Collect the market-creation fee in the treasury's native-token account.
        invoke(
            &system_instruction::transfer(
                ctx.accounts.payer.key,
                &ctx.accounts.team_treasury_wsol_account.key(),
                MARKET_CREATION_FEE_LAMPORTS,
            ),
            &[
                ctx.accounts.payer.to_account_info(),
                ctx.accounts.team_treasury_wsol_account.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
        )?;
        invoke(
            &spl_token::instruction::sync_native(
                ctx.accounts.token_program.key,
                &ctx.accounts.team_treasury_wsol_account.key(),
            )?,
            &[
                ctx.accounts.token_program.to_account_info(),
                ctx.accounts.team_treasury_wsol_account.to_account_info(),
            ],
        )?;

        let base_side = MarketSide {
            asset_mint: base_mint,
            asset_decimals: ctx.accounts.base_mint.decimals,
            hlp_mint: base_hlp_mint,
            reserve_vault: ctx.accounts.base_reserve_vault.key(),
            collateral_vault: base_collateral_vault,
            interest_vault: ctx.accounts.base_interest_vault.key(),
            ..MarketSide::default()
        };
        let quote_side = MarketSide {
            asset_mint: quote_mint,
            asset_decimals: ctx.accounts.quote_mint.decimals,
            hlp_mint: quote_hlp_mint,
            reserve_vault: ctx.accounts.quote_reserve_vault.key(),
            collateral_vault: quote_collateral_vault,
            interest_vault: ctx.accounts.quote_interest_vault.key(),
            ..MarketSide::default()
        };
        let base_hlp_ylp_vault = derive_hlp_ylp_vault_address(market_key, base_hlp_mint, ylp_mint).0;
        let quote_hlp_ylp_vault = derive_hlp_ylp_vault_address(market_key, quote_hlp_mint, ylp_mint).0;

        // Initialize all market state only after every external account is ready.
        ctx.accounts.market.initialize(
            ylp_mint,
            base_side,
            quote_side,
            args.config,
            base_hlp_ylp_vault,
            quote_hlp_ylp_vault,
            base_insurance_vault,
            quote_insurance_vault,
            args.params_hash,
            current_slot,
            ctx.bumps.market,
        )?;

        // Emit the complete immutable market identity and initial configuration.
        emit_cpi!(MarketCreated {
            market: market_key,
            base_mint,
            quote_mint,
            ylp_mint,
            base_collateral_vault,
            quote_collateral_vault,
            base_insurance_vault,
            quote_insurance_vault,
            base_hlp_mint,
            quote_hlp_mint,
            target_hlp_leverage_bps: args.config.target_hlp_leverage_bps,
            swap_fee_bps: args.config.swap_fee_bps,
            config: args.config,
            params_hash: args.params_hash,
            version: MARKET_LAYOUT_VERSION,
            metadata: MarketEventMetadata::new(payer_key, market_key)?,
        });

        Ok(())
    }
}

#[cfg(feature = "production")]
fn require_vanity_suffix(mint: &InterfaceAccount<Mint>, suffix: &str) -> Result<()> {
    let mint_key = mint.key().to_string();
    let start_idx = mint_key
        .len()
        .checked_sub(suffix.len())
        .ok_or(ErrorCode::InvalidLpMintKey)?;
    require_eq!(suffix, &mint_key[start_idx..], ErrorCode::InvalidLpMintKey);
    Ok(())
}

#[cfg(not(feature = "production"))]
fn require_vanity_suffix(_mint: &InterfaceAccount<Mint>, _suffix: &str) -> Result<()> {
    Ok(())
}

fn create_vault_token_account<'info>(
    market: &Account<'info, Market>,
    payer: &Signer<'info>,
    vault: &UncheckedAccount<'info>,
    mint: &InterfaceAccount<'info, Mint>,
    system_program: &Program<'info, System>,
    token_program: &AccountInfo<'info>,
    seed_prefix: &[u8],
    bump: u8,
) -> Result<()> {
    let market_key = market.key();
    let mint_key = mint.key();
    let bump_seed = [bump];
    create_token_account(
        &market.to_account_info(),
        &payer.to_account_info(),
        &vault.to_account_info(),
        &mint.to_account_info(),
        &system_program.to_account_info(),
        token_program,
        &[seed_prefix, market_key.as_ref(), mint_key.as_ref(), &bump_seed],
    )
}

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
        let InitializeLpMetadata {
            payer,
            market,
            lp_mint,
            lp_token_metadata,
            system_program,
            sysvar_instructions,
            token_2022_program,
            token_metadata_program,
        } = ctx.accounts;

        let token_metadata_program = token_metadata_program.to_account_info();
        let metadata = lp_token_metadata.to_account_info();
        let mint = lp_mint.to_account_info();
        let authority = market.to_account_info();
        let payer = payer.to_account_info();
        let system_program = system_program.to_account_info();
        let instructions_sysvar = sysvar_instructions.to_account_info();
        let token_2022_program = token_2022_program.to_account_info();
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
            .invoke_signed(&[&generate_market_seeds!(market)[..]])
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/instructions/market/initialize_lp_metadata.rs");
}
