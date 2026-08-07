use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LeveragePositionOpened, LeverageSwapEvent, MarketEventMetadata, ReferralBound},
    market::{leverage_debt_from_margin, LeverageSwapPlan, LeverageSwapQuote},
    shared::{
        account::{get_size_with_discriminator, initialize_pda_account_if_needed},
        token::{create_token_account, transfer_checked_with_remaining_accounts},
    },
    state::{FutarchyAuthority, LeveragePosition, Market, MarketAsset, ReferralAccrual, ReferralPartner},
};

use super::common::{
    leverage_collateral_credit, leverage_collateral_vault_pda, leverage_position_pda, leverage_swap_fee_credit,
    settle_inline_leverage_hlp, validate_leverage_collateral_risk_mint, validate_leverage_futarchy_pda,
    validate_leverage_market_pda, validate_leverage_mints, validate_leverage_reserve_accounts,
    validate_owner_debt_account,
};
use crate::instructions::common::{
    require_reserve_custody, token_account_credit, token_program_for_mint, HlpSwapAccountLayout,
};
use crate::instructions::referral::common::validate_referral_binding;
use crate::instructions::{SwapContext, SwapPlan};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct OpenLeverageArgs {
    pub position_id: Pubkey,
    pub debt_asset: u8,
    pub margin_amount: u64,
    pub multiplier_bps: u64,
    pub min_collateral_out: u64,
    pub referrer: Option<Pubkey>,
}

#[event_cpi]
#[derive(Accounts)]
pub struct OpenLeverage<'info> {
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,

    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    /// CHECK: Canonical PDA, ownership, allocation, and typed state are
    /// validated before any economic mutation.
    #[account(mut)]
    pub leverage_position: UncheckedAccount<'info>,

    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub debt_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub collateral_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    /// CHECK: Created lazily with the collateral mint's token program.
    pub leverage_collateral_vault: UncheckedAccount<'info>,

    #[account(mut)]
    pub owner_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub referral_partner: Option<Box<Account<'info, ReferralPartner>>>,

    pub referral_accrual: Option<Box<Account<'info, ReferralAccrual>>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl<'info> OpenLeverage<'info> {
    pub fn validate_at(&self, args: &OpenLeverageArgs, unix_timestamp: i64) -> Result<()> {
        validate_leverage_market_pda(&self.market, self.market.key())?;
        validate_leverage_futarchy_pda(self.futarchy_authority.bump, self.futarchy_authority.key())?;
        self.market
            .assert_live_with_futarchy_at(&self.futarchy_authority, unix_timestamp)?;
        require!(args.margin_amount > 0, ErrorCode::AmountZero);
        let (expected_position, _) = leverage_position_pda(self.market.key(), args.position_id)?;
        require_keys_eq!(
            self.leverage_position.key(),
            expected_position,
            ErrorCode::InvalidLeveragePosition
        );
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        validate_leverage_mints(&self.market, debt_asset, &self.debt_mint, &self.collateral_mint)?;
        validate_leverage_collateral_risk_mint(&self.collateral_mint)?;
        let (expected_collateral_vault, _) =
            leverage_collateral_vault_pda(self.market.key(), self.collateral_mint.key())?;
        require_keys_eq!(
            self.leverage_collateral_vault.key(),
            expected_collateral_vault,
            ErrorCode::InvalidVault
        );
        validate_leverage_reserve_accounts(
            &self.market,
            debt_asset,
            &self.debt_mint,
            &self.collateral_mint,
            &self.debt_reserve_vault,
            &self.collateral_reserve_vault,
        )?;
        validate_owner_debt_account(self.owner.key(), &self.debt_mint, &self.owner_debt_account)?;
        require_gte!(
            self.owner_debt_account.amount,
            args.margin_amount,
            ErrorCode::InsufficientBalance
        );
        validate_referral_binding(
            args.referrer,
            Pubkey::default(),
            0,
            false,
            &self.futarchy_authority,
            self.referral_partner.as_deref(),
            self.referral_accrual.as_deref(),
            self.market.key(),
            &self.debt_mint,
        )?;
        Ok(())
    }

    pub fn handle_open(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: OpenLeverageArgs,
        current_slot: u64,
        current_epoch: u64,
        unix_timestamp: i64,
    ) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let h_lp_accounts = {
            let market: &Market = &ctx.accounts.market;
            HlpSwapAccountLayout::try_from((market, ctx.remaining_accounts))?
        };
        let owner_key = ctx.accounts.owner.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let debt_mint_key = ctx.accounts.debt_mint.key();
        let collateral_mint_key = ctx.accounts.collateral_mint.key();
        let (expected_position, position_bump) = leverage_position_pda(market_key, args.position_id)?;
        require_keys_eq!(
            ctx.accounts.leverage_position.key(),
            expected_position,
            ErrorCode::InvalidLeveragePosition
        );
        let position_bump_seed = [position_bump];
        let position_seeds = [
            LEVERAGE_POSITION_SEED_PREFIX,
            market_key.as_ref(),
            args.position_id.as_ref(),
            &position_bump_seed,
        ];
        let initialized = initialize_pda_account_if_needed(
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.leverage_position.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            get_size_with_discriminator::<LeveragePosition>(),
            &position_seeds,
        )?;
        require!(initialized, ErrorCode::InvalidLeveragePosition);
        let mut leverage_position = {
            let data = ctx.accounts.leverage_position.try_borrow_data()?;
            let mut data_slice: &[u8] = &data;
            LeveragePosition::try_deserialize_unchecked(&mut data_slice)?
        };
        let (_, collateral_vault_bump) = leverage_collateral_vault_pda(market_key, collateral_mint_key)?;

        // Transfer margin into the debt reserve and measure its net credit.
        let debt_token_program = token_program_for_mint(
            &ctx.accounts.debt_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let reserve_balance_before = ctx.accounts.debt_reserve_vault.amount;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.owner_debt_account.to_account_info(),
            ctx.accounts.debt_reserve_vault.to_account_info(),
            ctx.accounts.debt_mint.to_account_info(),
            debt_token_program,
            args.margin_amount,
            ctx.accounts.debt_mint.decimals,
            &[],
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;
        ctx.accounts.debt_reserve_vault.reload()?;
        let margin_credit = token_account_credit(reserve_balance_before, &ctx.accounts.debt_reserve_vault)?;
        require!(margin_credit > 0, ErrorCode::AmountZero);

        // Quote the leveraged purchase against margin plus borrowed debt.
        let debt_amount = leverage_debt_from_margin(margin_credit, args.multiplier_bps)?;
        let referral = validate_referral_binding(
            args.referrer,
            Pubkey::default(),
            0,
            false,
            &ctx.accounts.futarchy_authority,
            ctx.accounts.referral_partner.as_deref(),
            ctx.accounts.referral_accrual.as_deref(),
            market_key,
            &ctx.accounts.debt_mint,
        )?;
        let notional = margin_credit
            .checked_add(debt_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let SwapPlan {
            quote,
            base_pre_rebalance,
            quote_pre_rebalance,
            fee_eligible_ylp_supply,
            interest_eligibility,
        } = SwapContext {
            current_slot,
            asset_in: debt_asset,
            reserve_credit: notional,
            reserved_daily_borrow: debt_amount,
        }
        .plan(&mut ctx.accounts.market)?;
        ctx.accounts.market.observe_current_risk(current_slot)?;
        let swap = LeverageSwapQuote::from_amm(quote, current_slot);
        let swap_plan = LeverageSwapPlan {
            swap,
            base_pre_rebalance,
            quote_pre_rebalance,
            fee_eligible_ylp_supply,
            interest_eligibility,
        };
        let collateral_credit =
            leverage_collateral_credit(&ctx.accounts.collateral_mint, swap.amount_out, current_epoch)?;
        require_gte!(collateral_credit, args.min_collateral_out, ErrorCode::SlippageExceeded);

        // Create the position vault and move the purchased collateral into custody.
        let collateral_token_program = token_program_for_mint(
            &ctx.accounts.collateral_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        create_token_account(
            &ctx.accounts.market.to_account_info(),
            &ctx.accounts.owner.to_account_info(),
            &ctx.accounts.leverage_collateral_vault.to_account_info(),
            &ctx.accounts.collateral_mint.to_account_info(),
            &ctx.accounts.system_program.to_account_info(),
            &collateral_token_program,
            &[
                LEVERAGE_COLLATERAL_VAULT_SEED_PREFIX,
                market_key.as_ref(),
                collateral_mint_key.as_ref(),
                &[collateral_vault_bump],
            ],
        )?;

        let swap_fee_credit = leverage_swap_fee_credit(&swap)?;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.collateral_reserve_vault.to_account_info(),
            ctx.accounts.leverage_collateral_vault.to_account_info(),
            ctx.accounts.collateral_mint.to_account_info(),
            collateral_token_program,
            swap.amount_out,
            ctx.accounts.collateral_mint.decimals,
            &[&crate::generate_market_seeds!(ctx.accounts.market)[..]],
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;

        // Commit position accounting and settle the resulting hLP exposure.
        let receipt = ctx.accounts.market.open_leverage(
            &mut leverage_position,
            owner_key,
            market_key,
            args.position_id,
            referral.referral_partner.unwrap_or_default(),
            referral.interest_share_bps,
            debt_asset,
            margin_credit,
            args.multiplier_bps,
            collateral_credit,
            swap_plan,
            swap_fee_credit,
            unix_timestamp,
            current_slot,
            position_bump,
            ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
        )?;
        {
            let mut data = ctx.accounts.leverage_position.try_borrow_mut_data()?;
            let mut data_slice: &mut [u8] = &mut data;
            leverage_position.try_serialize(&mut data_slice)?;
        }
        settle_inline_leverage_hlp(
            &mut ctx.accounts.market,
            &ctx.accounts.futarchy_authority,
            debt_asset,
            &ctx.accounts.debt_mint,
            &ctx.accounts.collateral_mint,
            &ctx.accounts.debt_reserve_vault,
            &ctx.accounts.collateral_reserve_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            ctx.remaining_accounts,
            h_lp_accounts,
            receipt.base_hlp_rebalance,
            receipt.quote_hlp_rebalance,
            interest_eligibility,
        )?;

        // Reconcile physical reserve custody after inline settlement.
        ctx.accounts.debt_reserve_vault.reload()?;
        ctx.accounts.collateral_reserve_vault.reload()?;
        require_reserve_custody(
            ctx.accounts.debt_reserve_vault.amount,
            ctx.accounts.market.side(debt_asset),
        )?;
        require_reserve_custody(
            ctx.accounts.collateral_reserve_vault.amount,
            ctx.accounts.market.side(debt_asset.opposite()),
        )?;

        let position_key = expected_position;

        // Emit the final position and referral state.
        emit_cpi!(LeveragePositionOpened {
            market: market_key,
            position: position_key,
            owner: owner_key,
            debt_asset_mint: debt_mint_key,
            collateral_asset_mint: collateral_mint_key,
            margin_amount: margin_credit,
            borrowed_amount: receipt.borrowed_amount,
            debt_amount: receipt.debt_amount,
            debt_shares: receipt.debt_shares,
            collateral_amount: receipt.collateral_amount,
            closeout_value: receipt.closeout_value,
            equity: receipt.equity,
            multiplier_bps: args.multiplier_bps,
            swap: LeverageSwapEvent::new(
                receipt.swap,
                swap_fee_credit,
                ctx.accounts.market.base_side.reserves.live_reserve,
                ctx.accounts.market.quote_side.reserves.live_reserve,
            )?,
            metadata: MarketEventMetadata::at_slot(owner_key, market_key, current_slot),
        });
        if let Some(referral_partner) = referral.referral_partner {
            emit_cpi!(ReferralBound {
                market: market_key,
                position: position_key,
                owner: owner_key,
                referrer: referral.referrer.ok_or(ErrorCode::InvalidReferralPartner)?,
                referral_partner,
                asset_mint: debt_mint_key,
                interest_share_bps: referral.interest_share_bps,
                metadata: MarketEventMetadata::at_slot(owner_key, market_key, current_slot),
            });
        }
        Ok(())
    }
}
