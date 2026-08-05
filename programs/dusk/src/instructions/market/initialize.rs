use anchor_lang::{
    prelude::*,
    solana_program::{program::invoke, system_instruction},
};
use anchor_spl::{
    token::{spl_token, Token, TokenAccount},
    token_interface::{Mint, Token2022},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{MarketCreated, MarketEventMetadata},
    shared::{account::get_size_with_discriminator, token::create_token_account},
    state::{FutarchyAuthority, Market, MarketConfig, MarketSide},
};

use crate::instructions::common::{
    derive_hlp_ylp_vault_address, require_supported_asset_mint, token_program_for_mint, validate_lp_mint,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeMarketArgs {
    pub operator: Pubkey,
    pub manager: Pubkey,
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

        // Default both roles to the deployer; an explicit non-default value in
        // args lets a deployer hand control to a multisig/operator at creation.
        let resolved_operator = if args.operator == Pubkey::default() {
            payer_key
        } else {
            args.operator
        };
        let resolved_manager = if args.manager == Pubkey::default() {
            payer_key
        } else {
            args.manager
        };

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

        ctx.accounts.market.initialize(
            ylp_mint,
            resolved_operator,
            resolved_manager,
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
            operator: resolved_operator,
            manager: resolved_manager,
            target_hlp_leverage_bps: args.config.target_hlp_leverage_bps,
            swap_fee_bps: args.config.swap_fee_bps,
            manager_fee_bps: args.config.manager_fee_bps,
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
