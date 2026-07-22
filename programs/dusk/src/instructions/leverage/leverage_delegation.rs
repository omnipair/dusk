use anchor_lang::prelude::*;

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LeverageDelegationUpdated, MarketEventMetadata},
    state::{LeverageDelegation, LeveragePosition, Market, MarketAsset},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateLeverageDelegationArgs {
    pub debt_asset: u8,
    pub delegated_program: Pubkey,
    pub approved_actions: u32,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateLeverageDelegationArgs {
    pub debt_asset: u8,
    pub delegated_program: Pubkey,
    pub approved_actions: u32,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CloseLeverageDelegationArgs {
    pub position: Pubkey,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: CreateLeverageDelegationArgs)]
pub struct CreateLeverageDelegation<'info> {
    #[account(
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(
        seeds = [
            LEVERAGE_POSITION_SEED_PREFIX,
            market.key().as_ref(),
            leverage_position.position_id.as_ref(),
        ],
        bump = leverage_position.bump,
        constraint = leverage_position.owner == owner.key() @ ErrorCode::InvalidLeveragePosition,
        constraint = leverage_position.market == market.key() @ ErrorCode::InvalidLeveragePosition,
        constraint = leverage_position.debt_asset == args.debt_asset @ ErrorCode::InvalidLeveragePosition,
    )]
    pub leverage_position: Box<Account<'info, LeveragePosition>>,

    #[account(
        init,
        payer = owner,
        space = 8 + LeverageDelegation::INIT_SPACE,
        seeds = [
            LEVERAGE_DELEGATION_SEED_PREFIX,
            leverage_position.key().as_ref(),
        ],
        bump
    )]
    pub leverage_delegation: Box<Account<'info, LeverageDelegation>>,

    #[account(mut)]
    pub owner: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: UpdateLeverageDelegationArgs)]
pub struct UpdateLeverageDelegation<'info> {
    #[account(
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(
        seeds = [
            LEVERAGE_POSITION_SEED_PREFIX,
            market.key().as_ref(),
            leverage_position.position_id.as_ref(),
        ],
        bump = leverage_position.bump,
        constraint = leverage_position.owner == owner.key() @ ErrorCode::InvalidLeveragePosition,
        constraint = leverage_position.market == market.key() @ ErrorCode::InvalidLeveragePosition,
        constraint = leverage_position.debt_asset == args.debt_asset @ ErrorCode::InvalidLeveragePosition,
    )]
    pub leverage_position: Box<Account<'info, LeveragePosition>>,

    #[account(
        mut,
        seeds = [
            LEVERAGE_DELEGATION_SEED_PREFIX,
            leverage_position.key().as_ref(),
        ],
        bump = leverage_delegation.bump,
        constraint = leverage_delegation.owner == owner.key() @ ErrorCode::InvalidLeverageDelegation,
        constraint = leverage_delegation.market == market.key() @ ErrorCode::InvalidLeverageDelegation,
        constraint = leverage_delegation.position == leverage_position.key() @ ErrorCode::InvalidLeverageDelegation,
        constraint = leverage_delegation.debt_asset == args.debt_asset @ ErrorCode::InvalidLeverageDelegation,
    )]
    pub leverage_delegation: Box<Account<'info, LeverageDelegation>>,

    #[account(mut)]
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(args: CloseLeverageDelegationArgs)]
pub struct CloseLeverageDelegation<'info> {
    #[account(
        mut,
        close = owner,
        seeds = [
            LEVERAGE_DELEGATION_SEED_PREFIX,
            args.position.as_ref(),
        ],
        bump = leverage_delegation.bump,
        constraint = leverage_delegation.owner == owner.key() @ ErrorCode::InvalidLeverageDelegation,
        constraint = leverage_delegation.position == args.position @ ErrorCode::InvalidLeverageDelegation,
    )]
    pub leverage_delegation: Box<Account<'info, LeverageDelegation>>,

    #[account(mut)]
    pub owner: Signer<'info>,
}

impl<'info> CreateLeverageDelegation<'info> {
    pub fn validate(&self, args: &CreateLeverageDelegationArgs) -> Result<()> {
        require_keys_neq!(
            args.delegated_program,
            Pubkey::default(),
            ErrorCode::InvalidLeverageDelegation
        );
        self.market.assert_started()?;
        self.leverage_position.require_open()?;
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        self.leverage_position
            .assert_position(self.owner.key(), self.market.key(), debt_asset)?;
        Ok(())
    }

    pub fn handle_create(ctx: Context<Self>, args: CreateLeverageDelegationArgs) -> Result<()> {
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let delegation = &mut ctx.accounts.leverage_delegation;
        delegation.initialize(
            ctx.accounts.owner.key(),
            ctx.accounts.market.key(),
            ctx.accounts.leverage_position.key(),
            debt_asset,
            args.delegated_program,
            args.approved_actions,
            ctx.bumps.leverage_delegation,
        );

        emit_cpi!(LeverageDelegationUpdated {
            market: ctx.accounts.market.key(),
            delegation: delegation.key(),
            position: ctx.accounts.leverage_position.key(),
            owner: ctx.accounts.owner.key(),
            delegated_program: args.delegated_program,
            approved_actions: args.approved_actions,
            metadata: MarketEventMetadata::new(ctx.accounts.owner.key(), ctx.accounts.market.key())?,
        });
        Ok(())
    }
}

impl<'info> UpdateLeverageDelegation<'info> {
    pub fn validate(&self, args: &UpdateLeverageDelegationArgs) -> Result<()> {
        require_keys_neq!(
            args.delegated_program,
            Pubkey::default(),
            ErrorCode::InvalidLeverageDelegation
        );
        self.market.assert_started()?;
        self.leverage_position.require_open()?;
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        self.leverage_position
            .assert_position(self.owner.key(), self.market.key(), debt_asset)?;
        self.leverage_delegation.assert_delegation(
            self.owner.key(),
            self.market.key(),
            self.leverage_position.key(),
            debt_asset,
        )?;
        Ok(())
    }

    pub fn handle_update(ctx: Context<Self>, args: UpdateLeverageDelegationArgs) -> Result<()> {
        let delegation = &mut ctx.accounts.leverage_delegation;
        delegation.update(args.delegated_program, args.approved_actions);

        emit_cpi!(LeverageDelegationUpdated {
            market: ctx.accounts.market.key(),
            delegation: delegation.key(),
            position: ctx.accounts.leverage_position.key(),
            owner: ctx.accounts.owner.key(),
            delegated_program: args.delegated_program,
            approved_actions: args.approved_actions,
            metadata: MarketEventMetadata::new(ctx.accounts.owner.key(), ctx.accounts.market.key())?,
        });
        Ok(())
    }
}

impl<'info> CloseLeverageDelegation<'info> {
    pub fn handle_close(_ctx: Context<Self>, _args: CloseLeverageDelegationArgs) -> Result<()> {
        Ok(())
    }
}
