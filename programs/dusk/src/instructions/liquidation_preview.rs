use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use crate::{
    constants::*,
    errors::ErrorCode,
    instructions::{
        accounts::require_supported_asset_mint, leverage_collateral_fee, leverage_collateral_liquidation_fee,
    },
    state::*,
    transitions::flash_liquidation::LiquidationPositionRef,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct PreviewFlashLiquidationArgs {
    pub debt_asset: u8,
    pub max_repayment: u64,
    pub full: bool,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct PreviewEmergencyLiquidationArgs {
    pub debt_asset: u8,
    pub collateral_debit: u64,
    pub full: bool,
}

#[derive(Accounts)]
pub struct PreviewFlashLiquidation<'info> {
    #[account(seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
    pub borrow_position: Option<Box<Account<'info, BorrowPosition>>>,
    pub leverage_position: Option<Box<Account<'info, LeveragePosition>>>,
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,
    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
}

#[derive(Accounts)]
pub struct PreviewEmergencyLiquidation<'info> {
    pub preview: PreviewFlashLiquidation<'info>,
    #[account(seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX], bump = futarchy_authority.bump)]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
}

#[event_cpi]
#[derive(Accounts)]
pub struct ObserveLiquidation<'info> {
    #[account(mut, seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut)]
    pub borrow_position: Option<Box<Account<'info, BorrowPosition>>>,
    #[account(mut)]
    pub leverage_position: Option<Box<Account<'info, LeveragePosition>>>,
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,
    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
}

#[event]
pub struct LiquidationObserved {
    pub market: Pubkey,
    pub position: Pubkey,
    pub debt_asset: u8,
    pub health: LiquidationHealth,
    pub distress: LiquidationDistress,
}

fn validate_position<'a>(
    market: &Account<Market>,
    borrow: &'a Option<Box<Account<BorrowPosition>>>,
    leverage: &'a Option<Box<Account<LeveragePosition>>>,
    asset: MarketAsset,
    collateral_mint: &InterfaceAccount<Mint>,
    debt_mint: &InterfaceAccount<Mint>,
) -> Result<LiquidationPositionRef<'a>> {
    market.assert_started()?;
    require_supported_asset_mint(collateral_mint)?;
    require_supported_asset_mint(debt_mint)?;
    require_keys_eq!(debt_mint.key(), market.side(asset).asset_mint, ErrorCode::InvalidMint);
    require_keys_eq!(
        collateral_mint.key(),
        market.side(asset.opposite()).asset_mint,
        ErrorCode::InvalidMint
    );
    let result = match (borrow, leverage) {
        (Some(p), None) => {
            require_keys_eq!(p.market, market.key(), ErrorCode::InvalidBorrowPosition);
            let key = Pubkey::create_program_address(
                &[
                    BORROW_POSITION_SEED_PREFIX,
                    p.market.as_ref(),
                    p.owner.as_ref(),
                    p.position_id.as_ref(),
                    &[p.bump],
                ],
                &crate::ID,
            )
            .map_err(|_| ErrorCode::InvalidBorrowPosition)?;
            require_keys_eq!(key, p.key(), ErrorCode::InvalidBorrowPosition);
            LiquidationPositionRef::Borrow(p)
        }
        (None, Some(p)) => {
            require_keys_eq!(p.market, market.key(), ErrorCode::InvalidLeveragePosition);
            require!(p.debt_asset()? == asset, ErrorCode::InvalidLeveragePosition);
            let key = Pubkey::create_program_address(
                &[
                    LEVERAGE_POSITION_SEED_PREFIX,
                    p.market.as_ref(),
                    p.owner.as_ref(),
                    p.namespace_authority.as_ref(),
                    p.position_id.as_ref(),
                    &[p.bump],
                ],
                &crate::ID,
            )
            .map_err(|_| ErrorCode::InvalidLeveragePosition)?;
            require_keys_eq!(key, p.key(), ErrorCode::InvalidLeveragePosition);
            LiquidationPositionRef::Leverage(p)
        }
        _ => return err!(ErrorCode::InvalidLiquidationSession),
    };
    result.require_idle()?;
    Ok(result)
}

impl PreviewEmergencyLiquidation<'_> {
    pub fn handle(
        ctx: Context<Self>,
        args: PreviewEmergencyLiquidationArgs,
    ) -> Result<crate::transitions::amm_liquidation::EmergencyLiquidationQuote> {
        let clock = Clock::get()?;
        let a = &mut ctx.accounts.preview;
        let asset = MarketAsset::try_from_code(args.debt_asset)?;
        a.market.accrue_interest()?;
        a.market.update()?;
        validate_position(
            &a.market,
            &a.borrow_position,
            &a.leverage_position,
            asset,
            &a.collateral_mint,
            &a.debt_mint,
        )?;
        let position = match (&a.borrow_position, &a.leverage_position) {
            (Some(p), None) => LiquidationPositionRef::Borrow(p),
            (None, Some(p)) => LiquidationPositionRef::Leverage(p),
            _ => return err!(ErrorCode::InvalidLiquidationSession),
        };
        Ok(a.market
            .prepare_emergency_liquidation(
                &position,
                asset,
                args.collateral_debit,
                args.full,
                leverage_collateral_fee(&a.collateral_mint, clock.epoch)?,
                leverage_collateral_liquidation_fee(&a.collateral_mint, clock.epoch)?,
                leverage_collateral_fee(&a.debt_mint, clock.epoch)?,
                clock.unix_timestamp,
                clock.slot,
                ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            )?
            .quote)
    }
}

impl PreviewFlashLiquidation<'_> {
    pub fn handle(ctx: Context<Self>, args: PreviewFlashLiquidationArgs) -> Result<FlashLiquidationQuote> {
        let clock = Clock::get()?;
        let a = &mut *ctx.accounts;
        let asset = MarketAsset::try_from_code(args.debt_asset)?;
        // The account is read-only: clock advancement exists only in this
        // deserialized copy and cannot start or reset a distress episode.
        a.market.accrue_interest()?;
        a.market.update()?;
        let position = validate_position(
            &a.market,
            &a.borrow_position,
            &a.leverage_position,
            asset,
            &a.collateral_mint,
            &a.debt_mint,
        )?;
        a.market.quote_flash_liquidation(
            &position,
            asset,
            args.max_repayment,
            args.full,
            leverage_collateral_fee(&a.collateral_mint, clock.epoch)?,
            leverage_collateral_liquidation_fee(&a.collateral_mint, clock.epoch)?,
            leverage_collateral_fee(&a.debt_mint, clock.epoch)?,
            clock.unix_timestamp,
            clock.slot,
        )
    }
}

impl ObserveLiquidation<'_> {
    pub fn handle(ctx: Context<Self>, debt_asset: u8) -> Result<LiquidationHealth> {
        let clock = Clock::get()?;
        let a = &mut *ctx.accounts;
        let asset = MarketAsset::try_from_code(debt_asset)?;
        crate::instructions::accounting::accrue_market_interest(
            &mut a.market,
            clock.slot,
            a.event_authority.to_account_info(),
        )?;
        a.market.update()?;
        let position = validate_position(
            &a.market,
            &a.borrow_position,
            &a.leverage_position,
            asset,
            &a.collateral_mint,
            &a.debt_mint,
        )?;
        let health = a.market.flash_liquidation_health(
            &position,
            asset,
            leverage_collateral_liquidation_fee(&a.collateral_mint, clock.epoch)?,
        )?;
        let (position, distress) = match (&mut a.borrow_position, &mut a.leverage_position) {
            (Some(p), None) => {
                p.distress_mut(asset)
                    .observe(health.eligible()?, clock.unix_timestamp, clock.slot)?;
                (p.key(), p.distress(asset))
            }
            (None, Some(p)) => {
                p.distress
                    .observe(health.eligible()?, clock.unix_timestamp, clock.slot)?;
                (p.key(), p.distress)
            }
            _ => return err!(ErrorCode::InvalidLiquidationSession),
        };
        let event = LiquidationObserved {
            market: a.market.key(),
            position,
            debt_asset: asset.code(),
            health,
            distress,
        };
        emit_cpi!(event);
        Ok(health)
    }
}
