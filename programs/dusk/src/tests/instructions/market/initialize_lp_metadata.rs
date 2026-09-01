use super::*;
use crate::{
    constants::{BPS_DENOMINATOR, MIN_HALF_LIFE_MS, NAD},
    state::{MarketConfig, MarketSide},
};

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

fn valid_metadata() -> InitializeLpMetadataArgs {
    InitializeLpMetadataArgs {
        name: "Omnipair V2 (Dusk) yLP".to_string(),
        symbol: "yLP".to_string(),
        uri: "https://omnipair.fi/metadata/dusk/ylp.json".to_string(),
    }
}

fn valid_config() -> MarketConfig {
    MarketConfig {
        swap_fee_bps: 30,
        divergence_fee_share_cap_bps: 0,
        volatility_fee_share_cap_bps: 0,
        target_hlp_leverage_bps: BPS_DENOMINATOR * 2,
        settlement_divergence_bps: 500,
        ema_half_life_ms: MIN_HALF_LIFE_MS,
        directional_ema_half_life_ms: MIN_HALF_LIFE_MS,
        curve_depth_ema_half_life_ms: MIN_HALF_LIFE_MS,
        max_daily_borrow_bps: 2_000,
        global_health_contribution_cap_bps: 15_000,
        borrow_market_health_floor_bps: 11_000,
        amm: Default::default(),
        irm: Default::default(),
        start_time: 0,
    }
}

struct MetadataMarketFixture {
    market: Market,
    initial_liquidity_authority: Pubkey,
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
    let initial_liquidity_authority = Pubkey::new_unique();
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
            base_side,
            quote_side,
            valid_config(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            [7; 32],
            initial_liquidity_authority,
            0,
            0,
            1,
            255,
        )
        .unwrap();
    MetadataMarketFixture {
        market,
        initial_liquidity_authority,
        ylp_mint,
        base_hlp_mint,
        quote_hlp_mint,
    }
}

#[test]
fn first_liquidity_seed_is_authorized_price_bound_and_then_permissionless() {
    let mut fixture = metadata_market();
    assert!(fixture
        .market
        .require_initial_liquidity_authority(Pubkey::new_unique())
        .is_err());
    fixture
        .market
        .require_initial_liquidity_authority(fixture.initial_liquidity_authority)
        .unwrap();

    fixture.market.amm.launch_reference_price_nad = NAD;
    let receipt = fixture.market.add_liquidity(1_000_000, 100_000_000).unwrap();
    assert_eq!(receipt.base_reserve_credit, 1_000_000);
    assert_eq!(receipt.quote_reserve_credit, 100_000_000);
    assert_eq!(fixture.market.amm.launch_reference_price_nad, NAD);
    assert_eq!(fixture.market.initial_liquidity_authority, Pubkey::default());
    fixture
        .market
        .require_initial_liquidity_authority(Pubkey::new_unique())
        .unwrap();
}

#[test]
fn first_liquidity_seed_rejects_a_mismatched_graduation_price_without_mutation() {
    let mut fixture = metadata_market();
    fixture.market.amm.launch_reference_price_nad = 2 * NAD;
    assert!(fixture.market.add_liquidity(1_000_000, 100_000_000).is_err());
    assert_eq!(fixture.market.base_side.reserves.live_reserve, 0);
    assert_eq!(fixture.market.base_side.reserves.cash_reserve, 0);
    assert_eq!(fixture.market.quote_side.reserves.live_reserve, 0);
    assert_eq!(fixture.market.quote_side.reserves.cash_reserve, 0);
    assert_eq!(fixture.market.base_side.shares.ylp_supply, 0);
    assert_eq!(
        fixture.market.initial_liquidity_authority,
        fixture.initial_liquidity_authority
    );
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
