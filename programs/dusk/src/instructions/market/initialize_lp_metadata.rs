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
        validate_lp_metadata(args)?;
        let decimals = lp_decimals_for_market_mint(&self.market, self.lp_mint.key())?;
        validate_lp_mint(&self.lp_mint, self.market.key(), decimals)?;
        require_vanity_suffix(&self.lp_mint, lp_vanity_suffix(&self.market, self.lp_mint.key())?)?;
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

fn validate_lp_metadata(metadata: &InitializeLpMetadataArgs) -> Result<()> {
    require!(metadata.name.len() <= 32, ErrorCode::InvalidLpName);
    require!(metadata.name.is_ascii(), ErrorCode::InvalidLpName);
    require!(metadata.symbol.len() <= 10, ErrorCode::InvalidLpSymbol);
    require!(metadata.symbol.is_ascii(), ErrorCode::InvalidLpSymbol);
    require!(metadata.uri.len() <= 200, ErrorCode::InvalidLpUri);
    require!(metadata.uri.starts_with("http"), ErrorCode::InvalidLpUri);
    Ok(())
}

fn lp_decimals_for_market_mint(market: &Market, lp_mint: Pubkey) -> Result<u8> {
    if lp_mint == market.ylp_mint || lp_mint == market.base_side.hlp_mint {
        return Ok(market.base_side.asset_decimals);
    }
    if lp_mint == market.quote_side.hlp_mint {
        return Ok(market.quote_side.asset_decimals);
    }
    err!(ErrorCode::InvalidLpMintKey)
}

fn lp_vanity_suffix(market: &Market, lp_mint: Pubkey) -> Result<&'static str> {
    if lp_mint == market.ylp_mint {
        return Ok("yLP");
    }
    if lp_mint == market.base_side.hlp_mint || lp_mint == market.quote_side.hlp_mint {
        return Ok("hLP");
    }
    err!(ErrorCode::InvalidLpMintKey)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        constants::{BPS_DENOMINATOR, MIN_HALF_LIFE_MS},
        state::{MarketConfig, MarketSide},
    };

    fn valid_metadata() -> InitializeLpMetadataArgs {
        InitializeLpMetadataArgs {
            name: "Omnipair Dusk (v2) yLP".to_string(),
            symbol: "yLP".to_string(),
            uri: "https://omnipair.fi/metadata/dusk/ylp.json".to_string(),
        }
    }

    fn valid_config() -> MarketConfig {
        MarketConfig {
            swap_fee_bps: 30,
            manager_fee_bps: 0,
            protocol_fee_bps: 0,
            target_hlp_leverage_bps: BPS_DENOMINATOR * 2,
            settlement_divergence_bps: 500,
            ema_half_life_ms: MIN_HALF_LIFE_MS,
            directional_ema_half_life_ms: MIN_HALF_LIFE_MS,
            k_ema_half_life_ms: MIN_HALF_LIFE_MS,
            max_daily_borrow_bps: 2_000,
            global_health_contribution_cap_bps: 15_000,
            borrow_market_health_floor_bps: 11_000,
            start_time: 0,
        }
    }

    struct MetadataMarketFixture {
        market: Market,
        ylp_mint: Pubkey,
        base_hlp_mint: Pubkey,
        quote_hlp_mint: Pubkey,
    }

    fn metadata_market() -> MetadataMarketFixture {
        let base_mint = Pubkey::new_unique();
        let quote_mint = Pubkey::new_unique();
        let ylp_mint = Pubkey::new_unique();
        let base_hlp_mint = Pubkey::new_unique();
        let quote_hlp_mint = Pubkey::new_unique();
        let base_side = MarketSide {
            asset_mint: base_mint,
            asset_decimals: 6,
            hlp_mint: base_hlp_mint,
            ..MarketSide::default()
        };
        let quote_side = MarketSide {
            asset_mint: quote_mint,
            asset_decimals: 8,
            hlp_mint: quote_hlp_mint,
            ..MarketSide::default()
        };
        let mut market = Market::default();
        market
            .initialize(
                ylp_mint,
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                base_side,
                quote_side,
                valid_config(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                [7; 32],
                1,
                255,
            )
            .unwrap();
        MetadataMarketFixture {
            market,
            ylp_mint,
            base_hlp_mint,
            quote_hlp_mint,
        }
    }

    #[test]
    fn lp_metadata_validation_accepts_valid_bounds() {
        let mut metadata = valid_metadata();
        metadata.name = "n".repeat(32);
        metadata.symbol = "s".repeat(10);
        metadata.uri = format!("http{}", "u".repeat(196));

        assert!(validate_lp_metadata(&metadata).is_ok());
    }

    #[test]
    fn lp_metadata_validation_rejects_oversized_or_non_ascii_values() {
        let mut metadata = valid_metadata();
        metadata.name = "n".repeat(33);
        assert!(validate_lp_metadata(&metadata).is_err());

        metadata = valid_metadata();
        metadata.name = "Omnipair Dusḱ".to_string();
        assert!(validate_lp_metadata(&metadata).is_err());

        metadata = valid_metadata();
        metadata.symbol = "yLPTOOLONG!".to_string();
        assert!(validate_lp_metadata(&metadata).is_err());

        metadata = valid_metadata();
        metadata.symbol = "γLP".to_string();
        assert!(validate_lp_metadata(&metadata).is_err());
    }

    #[test]
    fn lp_metadata_validation_rejects_bad_or_oversized_uri() {
        let mut metadata = valid_metadata();
        metadata.uri = "ipfs://omnipair/dusk/ylp.json".to_string();
        assert!(validate_lp_metadata(&metadata).is_err());

        metadata = valid_metadata();
        metadata.uri = format!("https://{}", "u".repeat(193));
        assert!(metadata.uri.len() > 200);
        assert!(validate_lp_metadata(&metadata).is_err());
    }

    #[test]
    fn lp_metadata_mint_classification_matches_market_lp_mints() {
        let fixture = metadata_market();

        assert_eq!(
            lp_decimals_for_market_mint(&fixture.market, fixture.ylp_mint).unwrap(),
            6
        );
        assert_eq!(
            lp_decimals_for_market_mint(&fixture.market, fixture.base_hlp_mint).unwrap(),
            6
        );
        assert_eq!(
            lp_decimals_for_market_mint(&fixture.market, fixture.quote_hlp_mint).unwrap(),
            8
        );
        assert_eq!(lp_vanity_suffix(&fixture.market, fixture.ylp_mint).unwrap(), "yLP");
        assert_eq!(lp_vanity_suffix(&fixture.market, fixture.base_hlp_mint).unwrap(), "hLP");
        assert_eq!(
            lp_vanity_suffix(&fixture.market, fixture.quote_hlp_mint).unwrap(),
            "hLP"
        );

        let unknown_mint = Pubkey::new_unique();
        assert!(lp_decimals_for_market_mint(&fixture.market, unknown_mint).is_err());
        assert!(lp_vanity_suffix(&fixture.market, unknown_mint).is_err());
    }
}
