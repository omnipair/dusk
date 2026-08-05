use anchor_lang::prelude::*;
use anchor_lang::solana_program::log::sol_log_data;
use anchor_lang::Discriminator;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::SwapExecuted,
    generate_market_seeds,
    shared::token::{get_transfer_fee_for_epoch, token_burn, token_mint_to, transfer_checked_with_remaining_accounts},
    state::{FutarchyAuthority, HlpRebalanceReceipt, Market, MarketAsset, SwapReceipt},
};

use crate::instructions::common::{
    require_reserve_custody, require_supported_asset_mint, token_account_info_amount, token_account_info_credit,
    token_program_for_mint, validate_swap_accounts, HlpSwapAccountLayout, BASE_HLP_YLP_VAULT_INDEX,
    BASE_INTEREST_VAULT_INDEX, HLP_SWAP_ACCOUNT_PREFIX_LEN, HLP_YLP_MINT_INDEX, QUOTE_HLP_YLP_VAULT_INDEX,
    QUOTE_INTEREST_VAULT_INDEX,
};
use crate::instructions::liquidity::record_inline_hlp_interest_credit;
use crate::instructions::{hlp_receipt_mutates_curve_inventory, split_claimable_fee_credit, SwapContext, SwapPlan};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SwapArgs {
    pub exact_asset_in: u64,
    pub min_asset_out: u64,
}

#[derive(Accounts)]
#[instruction(args: SwapArgs)]
pub struct Swap<'info> {
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

    #[account(
        seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump = futarchy_authority.bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    #[account(mut)]
    pub trader: Signer<'info>,

    pub asset_in_mint: Box<InterfaceAccount<'info, Mint>>,

    pub asset_out_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub reserve_in_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub reserve_out_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub trader_asset_in_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub trader_asset_out_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> Swap<'info> {
    pub fn validate_and_read_clock(&self, args: &SwapArgs) -> Result<(u64, u64)> {
        // Read the sysvar once for the complete swap. In particular, do not
        // call `Market::assert_started`, which would fetch `Clock` a second
        // time before the slot-driven AMM/debt pipeline begins.
        let clock = Clock::get()?;
        self.market.assert_current_version()?;
        require!(
            clock.unix_timestamp >= self.market.config.start_time,
            ErrorCode::MarketNotStarted
        );
        require!(
            !self.futarchy_authority.is_reduce_only(self.market.reduce_only),
            ErrorCode::ReduceOnlyMode
        );
        require!(args.exact_asset_in > 0, ErrorCode::AmountZero);
        require_gte!(
            self.trader_asset_in_account.amount,
            args.exact_asset_in,
            ErrorCode::InsufficientBalance
        );
        validate_swap_accounts(
            &self.market,
            self.trader.key(),
            &self.asset_in_mint,
            &self.asset_out_mint,
            &self.reserve_in_vault,
            &self.reserve_out_vault,
            &self.trader_asset_in_account,
            &self.trader_asset_out_account,
        )?;
        require_supported_asset_mint(&self.asset_in_mint)?;
        require_supported_asset_mint(&self.asset_out_mint)?;
        Ok((clock.slot, clock.epoch))
    }

    pub fn handle_swap(
        mut ctx: Context<'_, '_, '_, 'info, Self>,
        args: SwapArgs,
        current_slot: u64,
        current_epoch: u64,
    ) -> Result<()> {
        // The fixed hLP prefix is checked before transfer-fee, invariant,
        // controller, or hedge math. Only the trailing account slice is ever
        // offered to Token-2022 transfer-hook resolution.
        let h_lp_accounts = {
            let market: &Market = &ctx.accounts.market;
            HlpSwapAccountLayout::try_from((market, ctx.remaining_accounts))?
        };
        let market_key = ctx.accounts.market.key();
        let trader_key = ctx.accounts.trader.key();
        let asset_in = ctx.accounts.market.asset_for_mint(ctx.accounts.asset_in_mint.key())?;
        let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
        let protocol_fee_bps = ctx.accounts.futarchy_authority.revenue_share.swap_bps;
        let protocol_auction_split = ctx.accounts.futarchy_authority.protocol_auction_split;

        let reserve_credit = args
            .exact_asset_in
            .checked_sub(get_transfer_fee_for_epoch(
                &ctx.accounts.asset_in_mint.to_account_info(),
                args.exact_asset_in,
                current_epoch,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let SwapPlan {
            quote,
            base_pre_rebalance,
            quote_pre_rebalance,
            fee_eligible_ylp_supply,
            interest_eligibility,
        } = SwapContext {
            current_slot,
            asset_in,
            reserve_credit,
        }
        .plan(&mut ctx.accounts.market)?;
        let (base_fee_credit, distributed_surcharge_credit) =
            split_claimable_fee_credit(&quote.fee, quote.fee.claimable_fee_debit)?;

        let trade_endpoint = quote.trade_endpoint()?;
        let reserve_endpoint = quote.reserve_endpoint()?;
        let swap_receipt = {
            let market = &mut ctx.accounts.market;
            require_eq!(
                quote.fee.reserve_input_credit,
                quote
                    .fee
                    .amount_in_for_quote
                    .checked_add(quote.fee.retained_surcharge)
                    .ok_or(ErrorCode::ReserveOverflow)?,
                ErrorCode::BrokenInvariant
            );
            {
                let (side_in, side_out) = market.swap_sides_mut(quote.asset_in);
                require_gte!(
                    side_out.reserves.cash_reserve,
                    quote.amount_out,
                    ErrorCode::InsufficientLiquidity
                );
                side_in.credit_reserve(quote.fee.amount_in_for_quote, true)?;
                side_out.debit_reserve(quote.amount_out, true)?;
            }

            // The invariant-preserving trade and rounding dust are neutral.
            // Validate and reuse the quote-time endpoint instead of solving D/Q
            // again. Only a retained surcharge can increase protected budget.
            market.ensure_amm_initialized(current_slot)?;
            require!(market.amm.initialized, ErrorCode::BrokenInvariant);
            let evaluation = trade_endpoint.validated_evaluation(market, current_slot)?;
            let q_per_share_nad = market.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
            market.amm.commit_invariant(evaluation.invariant_d)?;
            market.amm.checkpoint_neutral_liquidity(q_per_share_nad);

            if quote.fee.retained_surcharge > 0 {
                market
                    .side_mut(quote.asset_in)
                    .credit_reserve(quote.fee.retained_surcharge, true)?;
                let evaluation = reserve_endpoint.validated_evaluation(market, current_slot)?;
                let q_per_share_nad = market.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
                market.amm.commit_invariant(evaluation.invariant_d)?;
                market.amm.checkpoint_retained_surcharge(q_per_share_nad)?;
            }

            let (reserve_in_live_reserve, reserve_out_live_reserve, fees) = {
                let (side_in, side_out) = market.swap_sides_mut(quote.asset_in);
                let fees = side_in.record_claimable_swap_fees(
                    base_fee_credit,
                    distributed_surcharge_credit,
                    manager_fee_bps,
                    protocol_fee_bps,
                    protocol_auction_split,
                    fee_eligible_ylp_supply,
                )?;
                side_in.assert_share_backing()?;
                side_out.assert_share_backing()?;
                side_in.fees.assert_backed()?;
                (side_in.reserves.live_reserve, side_out.reserves.live_reserve, fees)
            };
            SwapReceipt {
                amount_in_after_fee: quote.fee.amount_in_for_quote,
                reserve_input_credit: quote.fee.reserve_input_credit,
                amount_out: quote.amount_out,
                fee_credit: base_fee_credit
                    .checked_add(distributed_surcharge_credit)
                    .ok_or(ErrorCode::FeeMathOverflow)?,
                base_fee_credit,
                distributed_surcharge_credit,
                fee_breakdown: quote.fee,
                reserve_in_live_reserve,
                reserve_out_live_reserve,
                fees,
            }
        };
        ctx.accounts.market.finalize_amm_trade_after_inventory_checkpoint(
            quote.start_price_nad,
            quote.end_price_nad,
            current_slot,
        )?;
        let (base_rebalance, quote_rebalance) = ctx
            .accounts
            .market
            .finalize_hlp_vaults_for_swap(base_pre_rebalance, quote_pre_rebalance)?;
        let h_lp_tokens_will_change =
            rebalance_executes_token_changes(&base_rebalance) || rebalance_executes_token_changes(&quote_rebalance);
        let h_lp_mutates_curve_inventory = hlp_receipt_mutates_curve_inventory(&base_rebalance)
            || hlp_receipt_mutates_curve_inventory(&quote_rebalance);
        require!(
            !h_lp_tokens_will_change || h_lp_mutates_curve_inventory,
            ErrorCode::BrokenInvariant
        );
        let final_curve_evaluation = if h_lp_mutates_curve_inventory {
            // Internal hLP settlement changes executable inventory, so refresh
            // D/Q once and reuse that evaluation for the scalar risk mark.
            ctx.accounts.market.checkpoint_amm_neutral_inventory(current_slot)?
        } else if quote.fee.retained_surcharge > 0 {
            // The reserve endpoint was identity-validated when the retained
            // surcharge was checkpointed above. No state capable of changing
            // that identity has run since, so reuse its bound evaluation.
            quote.reserve_endpoint()?.evaluation()
        } else {
            // With no retained surcharge, the trade endpoint is also the
            // reserve endpoint and was identity-validated by the neutral
            // inventory checkpoint above.
            quote.trade_endpoint()?.evaluation()
        };
        // Advance EMA/Q state from the exact final endpoint, but leave the
        // pessimistic lending shapes stale until a risk-sensitive operation
        // materializes them. This avoids treating the pre-swap mark as the
        // observation for all time until the next borrow or liquidation.
        ctx.accounts
            .market
            .observe_risk_from_curve_evaluation(final_curve_evaluation, current_slot)?;

        // Final commit validation uses cached vault balances plus the known
        // net input, gross output, and gross hLP-interest deltas. This catches
        // unbacked executable cash or fee liabilities before any token CPI.
        let (base_vault_before, quote_vault_before) = match asset_in {
            MarketAsset::Base => (
                ctx.accounts.reserve_in_vault.amount,
                ctx.accounts.reserve_out_vault.amount,
            ),
            MarketAsset::Quote => (
                ctx.accounts.reserve_out_vault.amount,
                ctx.accounts.reserve_in_vault.amount,
            ),
        };
        let base_projected = projected_reserve_vault_balance(
            base_vault_before,
            if asset_in == MarketAsset::Base {
                reserve_credit
            } else {
                0
            },
            if asset_in == MarketAsset::Quote {
                quote.amount_out
            } else {
                0
            },
            quote_rebalance.interest_paid,
        )?;
        let quote_projected = projected_reserve_vault_balance(
            quote_vault_before,
            if asset_in == MarketAsset::Quote {
                reserve_credit
            } else {
                0
            },
            if asset_in == MarketAsset::Base {
                quote.amount_out
            } else {
                0
            },
            base_rebalance.interest_paid,
        )?;
        require_reserve_custody(base_projected, &ctx.accounts.market.base_side)?;
        require_reserve_custody(quote_projected, &ctx.accounts.market.quote_side)?;

        let asset_in_token_program = token_program_for_mint(
            &ctx.accounts.asset_in_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.trader.to_account_info(),
            ctx.accounts.trader_asset_in_account.to_account_info(),
            ctx.accounts.reserve_in_vault.to_account_info(),
            ctx.accounts.asset_in_mint.to_account_info(),
            asset_in_token_program,
            args.exact_asset_in,
            ctx.accounts.asset_in_mint.decimals,
            &[],
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;

        // One call per side and one net token settlement per receipt. A side
        // can mint or burn yLP, never both, and can transfer interest once.
        apply_single_hlp_rebalance_token_changes(
            &mut ctx,
            &base_rebalance,
            BASE_HLP_YLP_VAULT_INDEX,
            QUOTE_INTEREST_VAULT_INDEX,
            h_lp_accounts,
            interest_eligibility,
        )?;
        apply_single_hlp_rebalance_token_changes(
            &mut ctx,
            &quote_rebalance,
            QUOTE_HLP_YLP_VAULT_INDEX,
            BASE_INTEREST_VAULT_INDEX,
            h_lp_accounts,
            interest_eligibility,
        )?;

        let asset_out_token_program = token_program_for_mint(
            &ctx.accounts.asset_out_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.reserve_out_vault.to_account_info(),
            ctx.accounts.trader_asset_out_account.to_account_info(),
            ctx.accounts.asset_out_mint.to_account_info(),
            asset_out_token_program,
            quote.amount_out,
            ctx.accounts.asset_out_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;
        let asset_out_credit = quote
            .amount_out
            .checked_sub(get_transfer_fee_for_epoch(
                &ctx.accounts.asset_out_mint.to_account_info(),
                quote.amount_out,
                current_epoch,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(asset_out_credit, args.min_asset_out, ErrorCode::SlippageExceeded);

        // Serialize directly into one fixed stack buffer. Anchor's generic
        // event serializer allocates substantially more heap on this hot path.
        const SWAP_FEE_BREAKDOWN_EVENT_LEN: usize = 15 * 8;
        const SWAP_QUOTE_TELEMETRY_LEN: usize = SWAP_FEE_BREAKDOWN_EVENT_LEN + (7 * 8);
        const SWAP_EXECUTED_EVENT_LEN: usize = 8 + 32 + 32 + 1 + 8 + (2 * 16) + SWAP_QUOTE_TELEMETRY_LEN;
        let mut data = [0_u8; SWAP_EXECUTED_EVENT_LEN];
        let mut offset = 0usize;
        data[offset..offset + 8].copy_from_slice(SwapExecuted::DISCRIMINATOR);
        offset += 8;
        data[offset..offset + 32].copy_from_slice(market_key.as_ref());
        offset += 32;
        data[offset..offset + 32].copy_from_slice(trader_key.as_ref());
        offset += 32;
        data[offset] = asset_in.code();
        offset += 1;
        data[offset..offset + 8].copy_from_slice(&swap_receipt.amount_out.to_le_bytes());
        offset += 8;
        data[offset..offset + 16].copy_from_slice(&ctx.accounts.market.base_hlp_vault.residual_exposure.to_le_bytes());
        offset += 16;
        data[offset..offset + 16].copy_from_slice(&ctx.accounts.market.quote_hlp_vault.residual_exposure.to_le_bytes());
        offset += 16;
        macro_rules! write_quote_u64 {
            ($value:expr) => {{
                data[offset..offset + 8].copy_from_slice(&$value.to_le_bytes());
                offset += 8;
            }};
        }
        write_quote_u64!(quote.fee.reserve_credit);
        write_quote_u64!(quote.fee.base_fee_debit);
        write_quote_u64!(quote.fee.divergence_surcharge_debit);
        write_quote_u64!(quote.fee.volatility_surcharge_debit);
        write_quote_u64!(quote.fee.dynamic_surcharge_debit);
        write_quote_u64!(quote.fee.total_fee_debit);
        write_quote_u64!(quote.fee.retained_surcharge);
        write_quote_u64!(quote.fee.distributed_surcharge_debit);
        write_quote_u64!(quote.fee.amount_in_for_quote);
        write_quote_u64!(quote.fee.reserve_input_credit);
        write_quote_u64!(quote.fee.claimable_fee_debit);
        write_quote_u64!(quote.fee.base_fee_rate_nad);
        write_quote_u64!(quote.fee.divergence_fee_rate_nad);
        write_quote_u64!(quote.fee.volatility_fee_rate_nad);
        write_quote_u64!(quote.fee.total_fee_rate_nad);
        write_quote_u64!(quote.start_price_nad);
        write_quote_u64!(quote.end_price_nad);
        write_quote_u64!(quote.reserve_end_price_nad);
        write_quote_u64!(quote.decayed_volatility_nad);
        write_quote_u64!(quote.post_success_volatility_nad);
        write_quote_u64!(swap_receipt.base_fee_credit);
        write_quote_u64!(swap_receipt.distributed_surcharge_credit);
        let _ = offset;
        sol_log_data(&[&data]);

        Ok(())
    }
}

fn rebalance_executes_token_changes(receipt: &HlpRebalanceReceipt) -> bool {
    receipt.ylp_mint_amount > 0 || receipt.ylp_burn_amount > 0 || receipt.interest_paid > 0
}

fn apply_single_hlp_rebalance_token_changes<'info>(
    ctx: &mut anchor_lang::context::Context<'_, '_, '_, 'info, Swap<'info>>,
    receipt: &HlpRebalanceReceipt,
    ylp_vault_index: usize,
    interest_vault_index: usize,
    accounts: HlpSwapAccountLayout,
    interest_eligibility: crate::state::HlpYieldEligibility,
) -> Result<()> {
    if !rebalance_executes_token_changes(receipt) {
        return Ok(());
    }
    require_eq!(
        accounts.prefix_len,
        HLP_SWAP_ACCOUNT_PREFIX_LEN,
        ErrorCode::NotEnoughAccounts
    );
    require!(
        receipt.ylp_mint_amount == 0 || receipt.ylp_burn_amount == 0,
        ErrorCode::BrokenInvariant
    );
    let ylp_mint = &ctx.remaining_accounts[HLP_YLP_MINT_INDEX];
    let ylp_vault = &ctx.remaining_accounts[ylp_vault_index];
    let market_seeds = generate_market_seeds!(ctx.accounts.market);
    let signer_seeds = [&market_seeds[..]];

    if receipt.ylp_mint_amount > 0 {
        token_mint_to(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            ylp_mint.clone(),
            ylp_vault.clone(),
            receipt.ylp_mint_amount,
            &signer_seeds,
        )?;
    }
    if receipt.ylp_burn_amount > 0 {
        token_burn(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            ylp_mint.clone(),
            ylp_vault.clone(),
            receipt.ylp_burn_amount,
            &signer_seeds,
        )?;
    }
    if receipt.interest_paid > 0 {
        let borrowed_asset = receipt.target_asset.opposite();
        let interest_vault = &ctx.remaining_accounts[interest_vault_index];
        let interest_vault_balance_before = token_account_info_amount(interest_vault)?;
        let asset_in = ctx.accounts.market.asset_for_mint(ctx.accounts.asset_in_mint.key())?;
        let (reserve_vault, mint, token_program, decimals) = if asset_in == borrowed_asset {
            (
                ctx.accounts.reserve_in_vault.to_account_info(),
                ctx.accounts.asset_in_mint.to_account_info(),
                token_program_for_mint(
                    &ctx.accounts.asset_in_mint,
                    &ctx.accounts.token_program,
                    &ctx.accounts.token_2022_program,
                )?,
                ctx.accounts.asset_in_mint.decimals,
            )
        } else {
            (
                ctx.accounts.reserve_out_vault.to_account_info(),
                ctx.accounts.asset_out_mint.to_account_info(),
                token_program_for_mint(
                    &ctx.accounts.asset_out_mint,
                    &ctx.accounts.token_program,
                    &ctx.accounts.token_2022_program,
                )?,
                ctx.accounts.asset_out_mint.decimals,
            )
        };
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            reserve_vault,
            interest_vault.clone(),
            mint,
            token_program,
            receipt.interest_paid,
            decimals,
            &signer_seeds,
            accounts.hook_accounts(ctx.remaining_accounts),
        )?;
        let interest_vault_credit = token_account_info_credit(interest_vault_balance_before, interest_vault)?;
        let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
        record_inline_hlp_interest_credit(
            &mut ctx.accounts.market,
            borrowed_asset,
            interest_vault_credit,
            manager_fee_bps,
            ctx.accounts.futarchy_authority.revenue_share.interest_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
            interest_eligibility,
        )?;
    }
    Ok(())
}

fn projected_reserve_vault_balance(
    balance_before: u64,
    net_input_credit: u64,
    gross_swap_output: u64,
    gross_interest_debit: u64,
) -> Result<u64> {
    balance_before
        .checked_add(net_input_credit)
        .and_then(|value| value.checked_sub(gross_swap_output))
        .and_then(|value| value.checked_sub(gross_interest_debit))
        .ok_or_else(|| ErrorCode::UnbackedFeeLiability.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::program_option::COption;
    use spl_token_2022::{
        extension::{
            transfer_fee::{TransferFee, TransferFeeAmount},
            BaseStateWithExtensionsMut, ExtensionType, StateWithExtensionsMut,
        },
        state::{Account as SplToken2022Account, AccountState},
    };

    struct TestAccount {
        key: Pubkey,
        owner: Pubkey,
        writable: bool,
        lamports: u64,
        data: Vec<u8>,
    }

    impl TestAccount {
        fn new(key: Pubkey, owner: Pubkey, writable: bool) -> Self {
            Self {
                key,
                owner,
                writable,
                lamports: 1,
                data: vec![],
            }
        }

        fn account_info(&mut self) -> AccountInfo<'_> {
            AccountInfo::new(
                &self.key,
                false,
                self.writable,
                &mut self.lamports,
                self.data.as_mut_slice(),
                &self.owner,
                false,
                0,
            )
        }
    }

    fn market_requiring_hlp_swap_accounts() -> Market {
        let mut market = Market::default();
        market.ylp_mint = Pubkey::new_unique();
        market.base_hlp_vault.ylp_vault = Pubkey::new_unique();
        market.quote_hlp_vault.ylp_vault = Pubkey::new_unique();
        market.base_side.interest_vault = Pubkey::new_unique();
        market.quote_side.interest_vault = Pubkey::new_unique();
        market.base_hlp_vault.hlp_supply = 1;
        market
    }

    fn canonical_hlp_swap_accounts(market: &Market) -> Vec<TestAccount> {
        vec![
            TestAccount::new(market.ylp_mint, Token2022::id(), true),
            TestAccount::new(market.base_hlp_vault.ylp_vault, Token2022::id(), true),
            TestAccount::new(market.quote_hlp_vault.ylp_vault, Token2022::id(), true),
            TestAccount::new(market.base_side.interest_vault, Token::id(), true),
            TestAccount::new(market.quote_side.interest_vault, Token2022::id(), true),
        ]
    }

    #[test]
    fn inactive_hlp_leaves_every_remaining_account_available_to_transfer_hooks() {
        let market = Market::default();
        let hook_key = Pubkey::new_unique();
        let mut accounts = vec![TestAccount::new(hook_key, Pubkey::new_unique(), false)];
        let infos: Vec<_> = accounts.iter_mut().map(TestAccount::account_info).collect();

        let layout = HlpSwapAccountLayout::try_from((&market, infos.as_slice())).unwrap();

        assert_eq!(layout.prefix_len, 0);
        assert_eq!(layout.hook_accounts(&infos)[0].key(), hook_key);
    }

    #[test]
    fn residual_only_hlp_rejects_a_missing_or_reordered_prefix_before_math() {
        let mut market = market_requiring_hlp_swap_accounts();
        market.base_hlp_vault.hlp_supply = 0;
        market.base_hlp_vault.residual_exposure = 1;
        let no_accounts: &[AccountInfo<'_>] = &[];
        assert_eq!(
            HlpSwapAccountLayout::try_from((&market, no_accounts)).unwrap_err(),
            error!(ErrorCode::NotEnoughAccounts)
        );

        let mut accounts = canonical_hlp_swap_accounts(&market);
        accounts.swap(BASE_HLP_YLP_VAULT_INDEX, QUOTE_HLP_YLP_VAULT_INDEX);
        let infos: Vec<_> = accounts.iter_mut().map(TestAccount::account_info).collect();
        assert_eq!(
            HlpSwapAccountLayout::try_from((&market, infos.as_slice())).unwrap_err(),
            error!(ErrorCode::InvalidVault)
        );
    }

    #[test]
    fn canonical_hlp_prefix_is_consumed_once_and_preserves_hook_tail_order() {
        let market = market_requiring_hlp_swap_accounts();
        let first_hook = Pubkey::new_unique();
        let second_hook = Pubkey::new_unique();
        let mut accounts = canonical_hlp_swap_accounts(&market);
        accounts.push(TestAccount::new(first_hook, Pubkey::new_unique(), false));
        accounts.push(TestAccount::new(second_hook, Pubkey::new_unique(), true));
        let infos: Vec<_> = accounts.iter_mut().map(TestAccount::account_info).collect();

        let layout = HlpSwapAccountLayout::try_from((&market, infos.as_slice())).unwrap();
        let hook_accounts = layout.hook_accounts(&infos);

        assert_eq!(layout.prefix_len, HLP_SWAP_ACCOUNT_PREFIX_LEN);
        assert_eq!(hook_accounts.len(), 2);
        assert_eq!(hook_accounts[0].key(), first_hook);
        assert_eq!(hook_accounts[1].key(), second_hook);
    }

    #[test]
    fn inactive_hlp_side_accepts_its_uninitialized_derived_vault_but_active_side_does_not() {
        let market = market_requiring_hlp_swap_accounts();
        let mut accounts = canonical_hlp_swap_accounts(&market);
        accounts[QUOTE_HLP_YLP_VAULT_INDEX].owner = anchor_lang::system_program::ID;
        let infos: Vec<_> = accounts.iter_mut().map(TestAccount::account_info).collect();
        HlpSwapAccountLayout::try_from((&market, infos.as_slice())).unwrap();

        let mut accounts = canonical_hlp_swap_accounts(&market);
        accounts[BASE_HLP_YLP_VAULT_INDEX].owner = anchor_lang::system_program::ID;
        let infos: Vec<_> = accounts.iter_mut().map(TestAccount::account_info).collect();
        assert_eq!(
            HlpSwapAccountLayout::try_from((&market, infos.as_slice())).unwrap_err(),
            error!(ErrorCode::InvalidTokenProgram)
        );
    }

    #[test]
    fn canonical_hlp_prefix_rejects_readonly_settlement_accounts() {
        let market = market_requiring_hlp_swap_accounts();
        let mut accounts = canonical_hlp_swap_accounts(&market);
        accounts[BASE_INTEREST_VAULT_INDEX].writable = false;
        let infos: Vec<_> = accounts.iter_mut().map(TestAccount::account_info).collect();

        assert_eq!(
            HlpSwapAccountLayout::try_from((&market, infos.as_slice())).unwrap_err(),
            error!(ErrorCode::InvalidVault)
        );
    }

    #[test]
    fn legacy_spl_custody_projection_uses_the_full_input_credit() {
        let projected = projected_reserve_vault_balance(1_000_000, 100_000, 25_000, 5_000).unwrap();
        let mut side = crate::state::MarketSide::default();
        side.reserves.cash_reserve = 1_050_000;
        side.fees.swap_fee_custody_balance = 20_000;

        assert_eq!(projected, 1_070_000);
        require_reserve_custody(projected, &side).unwrap();
    }

    #[test]
    fn token_2022_custody_projection_uses_net_input_and_gross_debits() {
        let gross_input = 100_000;
        let transfer_fee = 3_000;
        let projected = projected_reserve_vault_balance(1_000_000, gross_input - transfer_fee, 25_000, 5_000).unwrap();
        let mut side = crate::state::MarketSide::default();
        side.reserves.cash_reserve = 1_047_000;
        side.fees.swap_fee_custody_balance = 20_000;

        assert_eq!(projected, 1_067_000);
        require_reserve_custody(projected, &side).unwrap();
        assert_eq!(
            require_reserve_custody(projected - 1, &side).unwrap_err(),
            error!(ErrorCode::UnbackedFeeLiability)
        );
    }

    #[test]
    fn token_2022_hlp_interest_reads_net_credit_from_remaining_vault_data() {
        let transfer_fee = TransferFee {
            epoch: 0_u64.into(),
            maximum_fee: u64::MAX.into(),
            transfer_fee_basis_points: 300_u16.into(),
        };
        let gross_interest_paid = 10_000;
        let balance_before = 40_000;
        let actual_credit = transfer_fee.calculate_post_fee_amount(gross_interest_paid).unwrap();
        let balance_after = balance_before + actual_credit;
        let account_len =
            ExtensionType::try_calculate_account_len::<SplToken2022Account>(&[ExtensionType::TransferFeeAmount])
                .unwrap();
        let mut account_data = vec![0_u8; account_len];
        {
            let mut account =
                StateWithExtensionsMut::<SplToken2022Account>::unpack_uninitialized(&mut account_data).unwrap();
            account.init_extension::<TransferFeeAmount>(true).unwrap();
            account.base = SplToken2022Account {
                mint: Pubkey::new_unique(),
                owner: Pubkey::new_unique(),
                amount: balance_after,
                delegate: COption::None,
                state: AccountState::Initialized,
                is_native: COption::None,
                delegated_amount: 0,
                close_authority: COption::None,
            };
            account.pack_base();
            account.init_account_type().unwrap();
        }
        let account_key = Pubkey::new_unique();
        let token_program = spl_token_2022::ID;
        let mut lamports = 1;
        let account_info = AccountInfo::new(
            &account_key,
            false,
            true,
            &mut lamports,
            &mut account_data,
            &token_program,
            false,
            0,
        );

        assert_eq!(
            token_account_info_credit(balance_before, &account_info).unwrap(),
            actual_credit
        );
        assert_eq!(actual_credit, 9_700);
        assert!(actual_credit < gross_interest_paid);
    }
}
