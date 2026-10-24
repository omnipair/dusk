use crate::{
    constants::MARKET_V2_SEED_PREFIX,
    errors::ErrorCode,
    generate_market_seeds,
    instructions::{
        accounts::{
            require_reserve_custody, require_supported_asset_mint, token_account_credit, token_program_for_mint,
            validate_interest_accounts, validate_side_vault_accounts, HlpSwapAccountLayout,
        },
        leverage::settlement::settle_new_public_hlp_income,
        validate_hlp_authority_pdas,
    },
    state::{Debt, FutarchyAuthority, Market, MarketAsset},
    token::transfer_checked_with_remaining_accounts,
    transitions::HlpYieldEligibility,
};
use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct FundInterestArgs {
    /// Gross public interest to fund, including the frozen protocol allocation.
    pub max_base_gross: u64,
    pub max_quote_gross: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Default)]
pub struct FundInterestReceipt {
    pub gross_funded: [u64; 2],
    pub lp_custody_credit: [u64; 2],
    /// [hLP target side][income side], after covering the funding gap.
    pub hlp_yield_published: [[u64; 2]; 2],
    pub hlp_capital_gross: [[u64; 2]; 2],
    pub hlp_capital_credit: [[u64; 2]; 2],
}

#[event]
pub struct PublicInterestFunded {
    pub market: Pubkey,
    pub caller: Pubkey,
    pub receipt: FundInterestReceipt,
}

/// Permissionless market-wide funding. It publishes cash-backed holder yield;
/// `harvest` then pays the individual account. Existing funded claims remain
/// independently harvestable if a new funding/rebalance transaction cannot run.
#[event_cpi]
#[derive(Accounts)]
pub struct FundInterest<'info> {
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
    pub caller: Signer<'info>,
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub base_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub base_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> FundInterest<'info> {
    pub fn handle(ctx: Context<'_, '_, '_, 'info, Self>, args: FundInterestArgs) -> Result<FundInterestReceipt> {
        let a = ctx.accounts;
        validate_hlp_authority_pdas(
            &a.market,
            a.market.key(),
            &a.futarchy_authority,
            a.futarchy_authority.key(),
        )?;
        a.market.assert_current_version()?;
        a.market.assert_started()?;
        require_keys_eq!(a.base_mint.key(), a.market.base_side.asset_mint, ErrorCode::InvalidMint);
        require_keys_eq!(
            a.quote_mint.key(),
            a.market.quote_side.asset_mint,
            ErrorCode::InvalidMint
        );
        for (mint, reserve, interest) in [
            (&a.base_mint, &a.base_reserve_vault, &a.base_interest_vault),
            (&a.quote_mint, &a.quote_reserve_vault, &a.quote_interest_vault),
        ] {
            require_supported_asset_mint(mint)?;
            validate_side_vault_accounts(&a.market, a.market.asset_for_mint(mint.key())?, mint, reserve)?;
            validate_interest_accounts(&a.market, mint, interest)?;
            require_keys_eq!(interest.owner, a.market.key(), ErrorCode::InvalidVault);
            require_keys_eq!(reserve.owner, a.market.key(), ErrorCode::InvalidVault);
        }
        let layout = HlpSwapAccountLayout::try_from((&**a.market, ctx.remaining_accounts))?;
        let clock = Clock::get()?;
        crate::instructions::accounting::accrue_market_interest(
            &mut a.market,
            clock.slot,
            a.event_authority.to_account_info(),
        )?;
        a.market.advance_amm_clock(clock.slot)?;
        a.market.checkpoint_hlp_yield_from_ylp(MarketAsset::Base)?;
        a.market.checkpoint_hlp_yield_from_ylp(MarketAsset::Quote)?;
        let eligibility = HlpYieldEligibility {
            ylp_supply: a.market.base_side.shares.ylp_supply,
            base_hlp_ylp_shares: a.market.base_hlp_vault.ylp_shares,
            quote_hlp_ylp_shares: a.market.quote_hlp_vault.ylp_shares,
        };
        let mut result = FundInterestReceipt::default();
        for (asset, mint, reserve, interest, maximum) in [
            (
                MarketAsset::Base,
                &a.base_mint,
                &mut a.base_reserve_vault,
                &mut a.base_interest_vault,
                args.max_base_gross,
            ),
            (
                MarketAsset::Quote,
                &a.quote_mint,
                &mut a.quote_reserve_vault,
                &mut a.quote_interest_vault,
                args.max_quote_gross,
            ),
        ] {
            let i = asset.code() as usize;
            let funding_vault = if asset == MarketAsset::Base {
                &a.market.quote_hlp_vault
            } else {
                &a.market.base_hlp_vault
            };
            let funding_interest = u64::try_from(Debt::shares_to_debt(
                funding_vault.debt_shares,
                a.market.debt.borrow_index(asset),
            )?)
            .map_err(|_| ErrorCode::DebtMathOverflow)?
            .saturating_sub(funding_vault.debt_principal);
            // Reserve cash already owed as funding interest is not free to advance.
            let available = a
                .market
                .side(asset)
                .reserves
                .cash_reserve
                .saturating_sub(funding_interest);
            let (gross, lp_gross) = a.market.fund_public_interest(
                asset,
                maximum.min(available),
                a.futarchy_authority.revenue_share.interest_bps,
            )?;
            let before = interest.amount;
            if lp_gross > 0 {
                transfer_checked_with_remaining_accounts(
                    a.market.to_account_info(),
                    reserve.to_account_info(),
                    interest.to_account_info(),
                    mint.to_account_info(),
                    token_program_for_mint(mint, &a.token_program, &a.token_2022_program)?,
                    lp_gross,
                    mint.decimals,
                    &[&generate_market_seeds!(a.market)[..]],
                    layout.hook_accounts(ctx.remaining_accounts),
                )?;
                interest.reload()?;
            }
            let credit = token_account_credit(before, interest)?;
            a.market.side_mut(asset).record_interest_credit_allocation(
                credit,
                0,
                a.futarchy_authority.protocol_auction_split,
                0,
                eligibility.ylp_supply,
            )?;
            result.gross_funded[i] = gross;
            result.lp_custody_credit[i] = credit;
        }
        if result.gross_funded != [0, 0] {
            let receipt = settle_new_public_hlp_income(
                &mut a.market,
                &a.futarchy_authority,
                &a.token_program,
                &a.token_2022_program,
                ctx.remaining_accounts,
                eligibility,
                a.event_authority.to_account_info(),
            )?;
            result.hlp_yield_published = receipt.published;
            result.hlp_capital_gross = receipt.reserve_debit;
            result.hlp_capital_credit = receipt.reserve_credit;
        }
        a.base_reserve_vault.reload()?;
        a.quote_reserve_vault.reload()?;
        a.base_interest_vault.reload()?;
        a.quote_interest_vault.reload()?;
        for (side, reserve, interest) in [
            (&a.market.base_side, &a.base_reserve_vault, &a.base_interest_vault),
            (&a.market.quote_side, &a.quote_reserve_vault, &a.quote_interest_vault),
        ] {
            require_reserve_custody(reserve.amount, side)?;
            require_gte!(
                interest.amount,
                side.fees.interest_vault_balance,
                ErrorCode::UnbackedFeeLiability
            );
        }
        a.market.assert_market_invariants()?;
        crate::instructions::accounting::emit_accounting_event(
            &PublicInterestFunded {
                market: a.market.key(),
                caller: a.caller.key(),
                receipt: result.clone(),
            },
            a.event_authority.to_account_info(),
        )?;
        Ok(result)
    }
}
