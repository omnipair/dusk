use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use crate::{
    constants::*,
    events::{MarketObserved, MarketObservedSide},
    instructions::governance::{direct_ylp_eligible_supply, governance_vault_amount},
    state::{Market, MarketAsset},
};

#[event_cpi]
#[derive(Accounts)]
pub struct ObserveMarket<'info> {
    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(address = market.ylp_mint)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,

    /// CHECK: Must be the market's canonical base-hLP yLP vault. Before its
    /// first use it is an empty System account holding no yLP.
    pub base_hlp_ylp_vault: UncheckedAccount<'info>,

    /// CHECK: Validated under the same rules as the base-hLP yLP vault.
    pub quote_hlp_ylp_vault: UncheckedAccount<'info>,
}

impl<'info> ObserveMarket<'info> {
    /// An observation moves no tokens and opens no exposure. Like the refresh
    /// that exits and previews run, it stays available before the market start
    /// time and in reduce-only mode.
    pub fn validate(&self) -> Result<()> {
        Ok(())
    }

    crate::instructions::accounts::market_update_and_validate!();

    pub fn handle_observe(ctx: Context<Self>) -> Result<()> {
        let accounts = &ctx.accounts;
        let market: &Market = &accounts.market;

        // Count eligible direct yLP exactly as proposal sponsorship does.
        let base_hlp_ylp_amount = governance_vault_amount(
            &accounts.market,
            &accounts.ylp_mint,
            &accounts.base_hlp_ylp_vault,
            market.base_hlp_vault.ylp_vault,
            market.base_hlp_vault.ylp_shares,
        )?;
        let quote_hlp_ylp_amount = governance_vault_amount(
            &accounts.market,
            &accounts.ylp_mint,
            &accounts.quote_hlp_ylp_vault,
            market.quote_hlp_vault.ylp_vault,
            market.quote_hlp_vault.ylp_shares,
        )?;

        let event = MarketObserved {
            market: accounts.market.key(),
            ylp_mint: market.ylp_mint,
            slot: Clock::get()?.slot,
            ylp_supply: market.base_side.shares.ylp_supply,
            governance_locked_ylp: market.governance_locked_ylp,
            eligible_ylp: direct_ylp_eligible_supply(
                market,
                accounts.ylp_mint.supply,
                base_hlp_ylp_amount,
                quote_hlp_ylp_amount,
            )?,
            base: observed_side(market, MarketAsset::Base)?,
            quote: observed_side(market, MarketAsset::Quote)?,
        };
        emit_cpi!(event);
        Ok(())
    }
}

fn observed_side(market: &Market, asset: MarketAsset) -> Result<MarketObservedSide> {
    let side = market.side(asset);
    let prices = market.side_prices(asset)?;
    Ok(MarketObservedSide {
        asset_mint: side.asset_mint,
        asset_decimals: side.asset_decimals,
        live_reserve: side.reserves.live_reserve,
        spot_price_nad: prices.spot_price_nad,
        price_ema_nad: prices.price_ema_nad,
        swap_fee_growth_index_q64: side.fees.swap_fee_growth_index_q64,
        interest_growth_index_q64: side.fees.interest_growth_index_q64,
        borrow_index_nad: market.debt.borrow_index(asset),
    })
}
