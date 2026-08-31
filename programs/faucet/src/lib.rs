use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    metadata::{
        mpl_token_metadata::{
            instructions::{CreateV1Cpi, CreateV1CpiAccounts, CreateV1InstructionArgs},
            types::TokenStandard,
            ID as MPL_TOKEN_METADATA_PROGRAM_ID,
        },
        Metadata,
    },
    token_interface::{self, Mint, MintTo, TokenAccount, TokenInterface},
};

declare_id!("EMmV9HKeQndxFd4duqp65rUSjikVWCPakBH1UjJJ32dz");

pub const FAUCET_ADMIN: Pubkey = pubkey!("FJWRK3XJeVD8njSvTFXyHwP2jkatvBqeVwcNAFe5zVfJ");

#[program]
pub mod faucet {
    use super::*;

    pub fn faucet_mint(ctx: Context<FaucetMint>, amount: u64) -> Result<()> {
        require_gt!(amount, 0, FaucetError::InvalidAmount);

        let seeds = &[
            b"faucet_authority",
            crate::ID.as_ref(),
            &[ctx.bumps.faucet_authority],
        ];
        let signer_seeds = &[&seeds[..]];

        token_interface::mint_to(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                MintTo {
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.recipient_token_account.to_account_info(),
                    authority: ctx.accounts.faucet_authority.to_account_info(),
                },
                signer_seeds,
            ),
            amount,
        )
    }

    pub fn initialize_mint_metadata(
        ctx: Context<InitializeMintMetadata>,
        args: InitializeMintMetadataArgs,
    ) -> Result<()> {
        require!(!args.name.is_empty(), FaucetError::InvalidMetadataName);
        require!(
            args.name.len() <= 32 && args.name.is_ascii(),
            FaucetError::InvalidMetadataName
        );
        require!(!args.symbol.is_empty(), FaucetError::InvalidMetadataSymbol);
        require!(
            args.symbol.len() <= 10 && args.symbol.is_ascii(),
            FaucetError::InvalidMetadataSymbol
        );
        require!(
            args.uri.len() <= 200 && args.uri.is_ascii(),
            FaucetError::InvalidMetadataUri
        );

        let seeds = &[
            b"faucet_authority",
            crate::ID.as_ref(),
            &[ctx.bumps.faucet_authority],
        ];

        let token_metadata_program = ctx.accounts.token_metadata_program.to_account_info();
        let metadata = ctx.accounts.metadata.to_account_info();
        let mint = ctx.accounts.mint.to_account_info();
        let faucet_authority = ctx.accounts.faucet_authority.to_account_info();
        let payer = ctx.accounts.payer.to_account_info();
        let system_program = ctx.accounts.system_program.to_account_info();
        let instructions_sysvar = ctx.accounts.sysvar_instructions.to_account_info();
        let token_program = ctx.accounts.token_program.to_account_info();
        let cpi_accounts = CreateV1CpiAccounts {
            metadata: &metadata,
            master_edition: None,
            mint: (&mint, false),
            authority: &faucet_authority,
            payer: &payer,
            update_authority: (&faucet_authority, true),
            system_program: &system_program,
            sysvar_instructions: &instructions_sysvar,
            spl_token_program: Some(&token_program),
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
            .invoke_signed(&[&seeds[..]])
            .map_err(Into::into)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeMintMetadataArgs {
    pub name: String,
    pub symbol: String,
    pub uri: String,
}

#[derive(Accounts)]
pub struct FaucetMint<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: May be any wallet or PDA that should receive the mock tokens.
    pub recipient: AccountInfo<'info>,

    /// CHECK: The seed-constrained PDA is the mint authority for every faucet mint.
    #[account(seeds = [b"faucet_authority", crate::ID.as_ref()], bump)]
    pub faucet_authority: AccountInfo<'info>,

    #[account(
        init_if_needed,
        payer = payer,
        associated_token::authority = recipient,
        associated_token::mint = mint,
        associated_token::token_program = token_program,
    )]
    pub recipient_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        mint::authority = faucet_authority,
        mint::token_program = token_program,
    )]
    pub mint: Box<InterfaceAccount<'info, Mint>>,

    pub system_program: Program<'info, System>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
}

#[derive(Accounts)]
pub struct InitializeMintMetadata<'info> {
    #[account(mut, address = FAUCET_ADMIN @ FaucetError::UnauthorizedMetadataAdmin)]
    pub payer: Signer<'info>,

    /// CHECK: The seed-constrained PDA is the mint and metadata authority.
    #[account(seeds = [b"faucet_authority", crate::ID.as_ref()], bump)]
    pub faucet_authority: AccountInfo<'info>,

    #[account(
        mut,
        mint::authority = faucet_authority,
        mint::token_program = token_program,
    )]
    pub mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        seeds = [
            b"metadata",
            MPL_TOKEN_METADATA_PROGRAM_ID.as_ref(),
            mint.key().as_ref(),
        ],
        seeds::program = MPL_TOKEN_METADATA_PROGRAM_ID,
        bump,
    )]
    /// CHECK: The Metaplex metadata PDA is constrained by the seeds above.
    pub metadata: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,

    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    /// CHECK: The Metaplex create_v1 CPI requires the instructions sysvar.
    pub sysvar_instructions: UncheckedAccount<'info>,

    pub token_program: Interface<'info, TokenInterface>,
    pub token_metadata_program: Program<'info, Metadata>,
}

#[error_code]
pub enum FaucetError {
    #[msg("Mint amount must be greater than zero")]
    InvalidAmount,
    #[msg("Only the configured faucet admin may initialize mock mint metadata")]
    UnauthorizedMetadataAdmin,
    #[msg("Mock mint metadata name must be 1-32 ASCII characters")]
    InvalidMetadataName,
    #[msg("Mock mint metadata symbol must be 1-10 ASCII characters")]
    InvalidMetadataSymbol,
    #[msg("Mock mint metadata URI must be at most 200 ASCII characters")]
    InvalidMetadataUri,
}
