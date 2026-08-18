use anchor_lang::prelude::*;
use anchor_spl::{
    token::{self, Token},
    token_2022::{self, Token2022},
    token_interface::{Mint, TokenAccount},
};
use dusk::{
    constants::{BPS_DENOMINATOR, LEVERAGE_MAX_MULTIPLIER_BPS, MARKET_LAYOUT_VERSION},
    instructions::{leverage_position_pda, OpenLeverageArgs, LEVERAGE_HLP_ACCOUNT_PREFIX_LEN},
    program::Dusk,
    state::{
        FutarchyAuthority, LeveragePosition, Market, MarketAsset, ReferralAccrual, ReferralPartner,
    },
};

use crate::{token_program_for_mint, LeverageDelegateError, ENTRY_ORDER_SEED_PREFIX};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateLeverageEntryOrderArgs {
    pub order_id: u64,
    pub position_id: Pubkey,
    pub debt_asset: u8,
    /// Gross amount transferred into escrow. The order records the measured
    /// net credit so Token-2022 transfer fees cannot underfund execution.
    pub deposit_amount: u64,
    pub min_margin_amount: u64,
    pub executor_bounty: u64,
    pub multiplier_bps: u64,
    /// Conservative all-in Quote-per-Base execution limit.
    pub limit_price_nad: u64,
    pub min_collateral_out: u64,
    pub expiry_unix_timestamp: i64,
    pub referrer: Option<Pubkey>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct LeverageEntryOrderIdArgs {
    pub order_id: u64,
}

#[account]
#[derive(InitSpace)]
pub struct LeverageEntryOrder {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub position: Pubkey,
    pub position_id: Pubkey,
    pub funding_vault: Pubkey,
    pub debt_mint: Pubkey,
    pub collateral_mint: Pubkey,
    pub order_id: u64,
    pub debt_asset: u8,
    /// Gross vault debit forwarded to Dusk as margin.
    pub margin_amount: u64,
    /// Gross vault debit paid to the successful executor.
    pub executor_bounty: u64,
    pub multiplier_bps: u64,
    pub limit_price_nad: u64,
    pub min_collateral_out: u64,
    pub expiry_unix_timestamp: i64,
    pub referrer: Option<Pubkey>,
    pub bump: u8,
}

#[derive(Accounts)]
#[instruction(args: CreateLeverageEntryOrderArgs)]
pub struct CreateLeverageEntryOrder<'info> {
    #[account(
        constraint = market.version == MARKET_LAYOUT_VERSION @ LeverageDelegateError::InvalidMarketVersion
    )]
    pub market: Box<Account<'info, Market>>,
    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init,
        payer = owner,
        space = 8 + LeverageEntryOrder::INIT_SPACE,
        seeds = [
            ENTRY_ORDER_SEED_PREFIX,
            market.key().as_ref(),
            owner.key().as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump
    )]
    pub order: Box<Account<'info, LeverageEntryOrder>>,
    #[account(
        mut,
        constraint = owner_funding_account.owner == owner.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_funding_account.mint == debt_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_funding_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = funding_vault.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = funding_vault.mint == debt_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub funding_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(args: LeverageEntryOrderIdArgs)]
pub struct CancelLeverageEntryOrder<'info> {
    #[account(
        mut,
        close = owner,
        seeds = [
            ENTRY_ORDER_SEED_PREFIX,
            order.market.as_ref(),
            owner.key().as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump = order.bump,
        constraint = order.owner == owner.key() @ LeverageDelegateError::InvalidOrder,
    )]
    pub order: Box<Account<'info, LeverageEntryOrder>>,
    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        address = order.funding_vault,
        constraint = funding_vault.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = funding_vault.mint == debt_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub funding_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = owner_funding_account.owner == owner.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_funding_account.mint == debt_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_funding_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

#[derive(Accounts)]
#[instruction(args: LeverageEntryOrderIdArgs)]
pub struct ExecuteLeverageEntryOrder<'info> {
    #[account(
        mut,
        close = owner,
        seeds = [
            ENTRY_ORDER_SEED_PREFIX,
            market.key().as_ref(),
            order.owner.as_ref(),
            &args.order_id.to_le_bytes(),
        ],
        bump = order.bump,
        constraint = order.market == market.key() @ LeverageDelegateError::InvalidOrder,
    )]
    pub order: Box<Account<'info, LeverageEntryOrder>>,
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
    /// CHECK: Receives order/vault rent and is bound by the stored order owner.
    #[account(mut, address = order.owner)]
    pub owner: AccountInfo<'info>,
    /// CHECK: Dusk initializes and validates the canonical position PDA.
    #[account(mut, address = order.position)]
    pub leverage_position: UncheckedAccount<'info>,
    #[account(address = order.debt_mint)]
    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address = order.collateral_mint)]
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub debt_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub collateral_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: Dusk validates/initializes the canonical shared collateral vault.
    #[account(mut)]
    pub leverage_collateral_vault: UncheckedAccount<'info>,
    #[account(
        mut,
        address = order.funding_vault,
        constraint = funding_vault.owner == order.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = funding_vault.mint == debt_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub funding_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = owner_refund_account.owner == order.owner @ LeverageDelegateError::InvalidTokenAccount,
        constraint = owner_refund_account.mint == debt_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub owner_refund_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = executor_bounty_account.owner == executor.key() @ LeverageDelegateError::InvalidTokenAccount,
        constraint = executor_bounty_account.mint == debt_mint.key() @ LeverageDelegateError::InvalidTokenAccount,
    )]
    pub executor_bounty_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub referral_partner: Option<Box<Account<'info, ReferralPartner>>>,
    pub referral_accrual: Option<Box<Account<'info, ReferralAccrual>>>,
    /// CHECK: Canonical Instructions sysvar, revalidated by Dusk.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
    #[account(mut)]
    pub executor: Signer<'info>,
    /// CHECK: Canonical Dusk CPI-event authority.
    #[account(seeds = [b"__event_authority"], bump, seeds::program = dusk::ID)]
    pub dusk_event_authority: AccountInfo<'info>,
    pub dusk_program: Program<'info, Dusk>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl<'info> CreateLeverageEntryOrder<'info> {
    pub fn handle_create(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: CreateLeverageEntryOrderArgs,
    ) -> Result<()> {
        require!(
            args.deposit_amount > 0
                && args.min_margin_amount > 0
                && args.limit_price_nad > 0
                && args.multiplier_bps > BPS_DENOMINATOR as u64
                && args.multiplier_bps <= LEVERAGE_MAX_MULTIPLIER_BPS,
            LeverageDelegateError::InvalidOrder
        );
        let now = Clock::get()?.unix_timestamp;
        require!(
            args.expiry_unix_timestamp > now,
            LeverageDelegateError::InvalidOrder
        );
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        require_keys_eq!(
            ctx.accounts.market.side(debt_asset).asset_mint,
            ctx.accounts.debt_mint.key(),
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_eq!(
            ctx.accounts.market.side(debt_asset.opposite()).asset_mint,
            ctx.accounts.collateral_mint.key(),
            LeverageDelegateError::InvalidTokenAccount
        );
        let (position, _) = leverage_position_pda(ctx.accounts.market.key(), args.position_id)?;

        let vault_balance_before = ctx.accounts.funding_vault.amount;
        transfer_checked(
            token_program_for_mint(
                &ctx.accounts.debt_mint.to_account_info(),
                &ctx.accounts.token_program.to_account_info(),
                &ctx.accounts.token_2022_program.to_account_info(),
            ),
            ctx.accounts.owner_funding_account.to_account_info(),
            ctx.accounts.debt_mint.to_account_info(),
            ctx.accounts.funding_vault.to_account_info(),
            ctx.accounts.owner.to_account_info(),
            args.deposit_amount,
            ctx.accounts.debt_mint.decimals,
            &[],
            ctx.remaining_accounts,
        )?;
        ctx.accounts.funding_vault.reload()?;
        let credited = ctx
            .accounts
            .funding_vault
            .amount
            .checked_sub(vault_balance_before)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        let margin_amount =
            escrow_margin_after_bounty(credited, args.executor_bounty, args.min_margin_amount)?;

        let order = &mut ctx.accounts.order;
        order.owner = ctx.accounts.owner.key();
        order.market = ctx.accounts.market.key();
        order.position = position;
        order.position_id = args.position_id;
        order.funding_vault = ctx.accounts.funding_vault.key();
        order.debt_mint = ctx.accounts.debt_mint.key();
        order.collateral_mint = ctx.accounts.collateral_mint.key();
        order.order_id = args.order_id;
        order.debt_asset = args.debt_asset;
        order.margin_amount = margin_amount;
        order.executor_bounty = args.executor_bounty;
        order.multiplier_bps = args.multiplier_bps;
        order.limit_price_nad = args.limit_price_nad;
        order.min_collateral_out = args.min_collateral_out;
        order.expiry_unix_timestamp = args.expiry_unix_timestamp;
        order.referrer = args.referrer;
        order.bump = ctx.bumps.order;
        Ok(())
    }
}

impl<'info> CancelLeverageEntryOrder<'info> {
    pub fn handle_cancel(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: LeverageEntryOrderIdArgs,
    ) -> Result<()> {
        require_keys_eq!(
            ctx.accounts.debt_mint.key(),
            ctx.accounts.order.debt_mint,
            LeverageDelegateError::InvalidTokenAccount
        );
        let market_key = ctx.accounts.order.market;
        let owner_key = ctx.accounts.order.owner;
        let order_id_bytes = args.order_id.to_le_bytes();
        let bump_seed = [ctx.accounts.order.bump];
        let authority_seeds = &[
            ENTRY_ORDER_SEED_PREFIX,
            market_key.as_ref(),
            owner_key.as_ref(),
            &order_id_bytes,
            &bump_seed,
        ];
        let signer = &[&authority_seeds[..]];
        let amount = ctx.accounts.funding_vault.amount;
        if amount > 0 {
            transfer_checked(
                token_program_for_mint(
                    &ctx.accounts.debt_mint.to_account_info(),
                    &ctx.accounts.token_program.to_account_info(),
                    &ctx.accounts.token_2022_program.to_account_info(),
                ),
                ctx.accounts.funding_vault.to_account_info(),
                ctx.accounts.debt_mint.to_account_info(),
                ctx.accounts.owner_funding_account.to_account_info(),
                ctx.accounts.order.to_account_info(),
                amount,
                ctx.accounts.debt_mint.decimals,
                signer,
                ctx.remaining_accounts,
            )?;
        }
        ctx.accounts.funding_vault.reload()?;
        require_eq!(
            ctx.accounts.funding_vault.amount,
            0,
            LeverageDelegateError::InvalidTokenAccount
        );
        close_token_account(
            token_program_for_mint(
                &ctx.accounts.debt_mint.to_account_info(),
                &ctx.accounts.token_program.to_account_info(),
                &ctx.accounts.token_2022_program.to_account_info(),
            ),
            ctx.accounts.funding_vault.to_account_info(),
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.order.to_account_info(),
            signer,
        )
    }
}

impl<'info> ExecuteLeverageEntryOrder<'info> {
    pub fn handle_execute(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: LeverageEntryOrderIdArgs,
    ) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        require!(
            now <= ctx.accounts.order.expiry_unix_timestamp,
            LeverageDelegateError::InvalidOrder
        );
        require!(
            ctx.accounts.market.version == MARKET_LAYOUT_VERSION,
            LeverageDelegateError::InvalidMarketVersion
        );
        let expected_escrow = ctx
            .accounts
            .order
            .margin_amount
            .checked_add(ctx.accounts.order.executor_bounty)
            .ok_or(LeverageDelegateError::MathOverflow)?;
        require_gte!(
            ctx.accounts.funding_vault.amount,
            expected_escrow,
            LeverageDelegateError::InvalidTokenAccount
        );
        let debt_asset = MarketAsset::try_from_code(ctx.accounts.order.debt_asset)?;
        let hook_account_offset = if ctx.accounts.market.has_active_hlp() {
            LEVERAGE_HLP_ACCOUNT_PREFIX_LEN
        } else {
            0
        };
        require_keys_eq!(
            ctx.accounts.market.side(debt_asset).asset_mint,
            ctx.accounts.debt_mint.key(),
            LeverageDelegateError::InvalidTokenAccount
        );
        require_keys_eq!(
            ctx.accounts.market.side(debt_asset.opposite()).asset_mint,
            ctx.accounts.collateral_mint.key(),
            LeverageDelegateError::InvalidTokenAccount
        );

        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.order.owner;
        let order_id_bytes = args.order_id.to_le_bytes();
        let bump_seed = [ctx.accounts.order.bump];
        let authority_seeds = &[
            ENTRY_ORDER_SEED_PREFIX,
            market_key.as_ref(),
            owner_key.as_ref(),
            &order_id_bytes,
            &bump_seed,
        ];
        let signer = &[&authority_seeds[..]];

        dusk::cpi::open_leverage(
            CpiContext::new_with_signer(
                ctx.accounts.dusk_program.to_account_info(),
                dusk::cpi::accounts::OpenLeverage {
                    market: ctx.accounts.market.to_account_info(),
                    futarchy_authority: ctx.accounts.futarchy_authority.to_account_info(),
                    owner: ctx.accounts.order.to_account_info(),
                    payer: ctx.accounts.executor.to_account_info(),
                    leverage_position: ctx.accounts.leverage_position.to_account_info(),
                    debt_mint: ctx.accounts.debt_mint.to_account_info(),
                    collateral_mint: ctx.accounts.collateral_mint.to_account_info(),
                    debt_reserve_vault: ctx.accounts.debt_reserve_vault.to_account_info(),
                    collateral_reserve_vault: ctx
                        .accounts
                        .collateral_reserve_vault
                        .to_account_info(),
                    leverage_collateral_vault: ctx
                        .accounts
                        .leverage_collateral_vault
                        .to_account_info(),
                    owner_debt_account: ctx.accounts.funding_vault.to_account_info(),
                    referral_partner: ctx
                        .accounts
                        .referral_partner
                        .as_ref()
                        .map(|account| account.to_account_info()),
                    referral_accrual: ctx
                        .accounts
                        .referral_accrual
                        .as_ref()
                        .map(|account| account.to_account_info()),
                    instructions_sysvar: ctx.accounts.instructions_sysvar.to_account_info(),
                    token_program: ctx.accounts.token_program.to_account_info(),
                    token_2022_program: ctx.accounts.token_2022_program.to_account_info(),
                    system_program: ctx.accounts.system_program.to_account_info(),
                    event_authority: ctx.accounts.dusk_event_authority.to_account_info(),
                    program: ctx.accounts.dusk_program.to_account_info(),
                },
                signer,
            )
            .with_remaining_accounts(ctx.remaining_accounts.to_vec()),
            OpenLeverageArgs {
                position_id: ctx.accounts.order.position_id,
                debt_asset: ctx.accounts.order.debt_asset,
                margin_amount: ctx.accounts.order.margin_amount,
                multiplier_bps: ctx.accounts.order.multiplier_bps,
                min_collateral_out: ctx.accounts.order.min_collateral_out,
                referrer: ctx.accounts.order.referrer,
                position_owner: Some(ctx.accounts.order.owner),
                limit_price_nad: ctx.accounts.order.limit_price_nad,
            },
        )?;

        ctx.accounts.funding_vault.reload()?;
        require_gte!(
            ctx.accounts.funding_vault.amount,
            ctx.accounts.order.executor_bounty,
            LeverageDelegateError::InvalidTokenAccount
        );
        verify_opened_position(&ctx.accounts.leverage_position, &ctx.accounts.order)?;

        let hook_accounts = ctx
            .remaining_accounts
            .get(hook_account_offset..)
            .ok_or(LeverageDelegateError::InvalidOrder)?;
        if ctx.accounts.order.executor_bounty > 0 {
            transfer_checked(
                token_program_for_mint(
                    &ctx.accounts.debt_mint.to_account_info(),
                    &ctx.accounts.token_program.to_account_info(),
                    &ctx.accounts.token_2022_program.to_account_info(),
                ),
                ctx.accounts.funding_vault.to_account_info(),
                ctx.accounts.debt_mint.to_account_info(),
                ctx.accounts.executor_bounty_account.to_account_info(),
                ctx.accounts.order.to_account_info(),
                ctx.accounts.order.executor_bounty,
                ctx.accounts.debt_mint.decimals,
                signer,
                hook_accounts,
            )?;
        }
        ctx.accounts.funding_vault.reload()?;
        let surplus = ctx.accounts.funding_vault.amount;
        if surplus > 0 {
            transfer_checked(
                token_program_for_mint(
                    &ctx.accounts.debt_mint.to_account_info(),
                    &ctx.accounts.token_program.to_account_info(),
                    &ctx.accounts.token_2022_program.to_account_info(),
                ),
                ctx.accounts.funding_vault.to_account_info(),
                ctx.accounts.debt_mint.to_account_info(),
                ctx.accounts.owner_refund_account.to_account_info(),
                ctx.accounts.order.to_account_info(),
                surplus,
                ctx.accounts.debt_mint.decimals,
                signer,
                hook_accounts,
            )?;
            ctx.accounts.funding_vault.reload()?;
        }
        require_eq!(
            ctx.accounts.funding_vault.amount,
            0,
            LeverageDelegateError::InvalidTokenAccount
        );
        close_token_account(
            token_program_for_mint(
                &ctx.accounts.debt_mint.to_account_info(),
                &ctx.accounts.token_program.to_account_info(),
                &ctx.accounts.token_2022_program.to_account_info(),
            ),
            ctx.accounts.funding_vault.to_account_info(),
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.order.to_account_info(),
            signer,
        )
    }
}

fn verify_opened_position(position: &UncheckedAccount, order: &LeverageEntryOrder) -> Result<()> {
    require_keys_eq!(
        *position.owner,
        dusk::ID,
        LeverageDelegateError::InvalidOrder
    );
    let data = position.try_borrow_data()?;
    let mut data_slice: &[u8] = &data;
    let position = LeveragePosition::try_deserialize(&mut data_slice)
        .map_err(|_| LeverageDelegateError::InvalidOrder)?;
    require_keys_eq!(
        position.owner,
        order.owner,
        LeverageDelegateError::InvalidOrder
    );
    require_keys_eq!(
        position.market,
        order.market,
        LeverageDelegateError::InvalidOrder
    );
    require_keys_eq!(
        position.position_id,
        order.position_id,
        LeverageDelegateError::InvalidOrder
    );
    require!(
        position.debt_asset == order.debt_asset,
        LeverageDelegateError::InvalidOrder
    );
    Ok(())
}

pub(crate) fn escrow_margin_after_bounty(
    credited_amount: u64,
    executor_bounty: u64,
    minimum_margin: u64,
) -> Result<u64> {
    let margin = credited_amount
        .checked_sub(executor_bounty)
        .ok_or(LeverageDelegateError::InvalidOrder)?;
    require_gte!(margin, minimum_margin, LeverageDelegateError::InvalidOrder);
    require!(margin > 0, LeverageDelegateError::InvalidOrder);
    Ok(margin)
}

#[allow(clippy::too_many_arguments)]
fn transfer_checked<'info>(
    token_program: AccountInfo<'info>,
    from: AccountInfo<'info>,
    mint: AccountInfo<'info>,
    to: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    amount: u64,
    decimals: u8,
    signer_seeds: &[&[&[u8]]],
    remaining_accounts: &[AccountInfo<'info>],
) -> Result<()> {
    if *token_program.key == Token2022::id() {
        token_2022::transfer_checked(
            CpiContext::new_with_signer(
                token_program,
                token_2022::TransferChecked {
                    from,
                    mint,
                    to,
                    authority,
                },
                signer_seeds,
            )
            .with_remaining_accounts(remaining_accounts.to_vec()),
            amount,
            decimals,
        )
    } else {
        token::transfer_checked(
            CpiContext::new_with_signer(
                token_program,
                token::TransferChecked {
                    from,
                    mint,
                    to,
                    authority,
                },
                signer_seeds,
            ),
            amount,
            decimals,
        )
    }
}

fn close_token_account<'info>(
    token_program: AccountInfo<'info>,
    account: AccountInfo<'info>,
    destination: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    if *token_program.key == Token2022::id() {
        token_2022::close_account(CpiContext::new_with_signer(
            token_program,
            token_2022::CloseAccount {
                account,
                destination,
                authority,
            },
            signer_seeds,
        ))
    } else {
        token::close_account(CpiContext::new_with_signer(
            token_program,
            token::CloseAccount {
                account,
                destination,
                authority,
            },
            signer_seeds,
        ))
    }
}
