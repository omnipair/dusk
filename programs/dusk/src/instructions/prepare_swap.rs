use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions::{
    load_current_index_checked, load_instruction_at_checked, ID as INSTRUCTIONS_SYSVAR_ID,
};
use anchor_lang::Discriminator;

use crate::{
    errors::ErrorCode,
    market::{
        liquidity::{prepare_explicit_hlp_transition, ExplicitHlpTransition, SwapCashPolicy},
        AmmSwapQuote, HlpRebalanceReceipt, SwapFeeBreakdown,
    },
    state::{HlpYieldEligibility, Market, MarketAsset, ProtocolAuctionSplit},
};

/// All state-derived inputs frozen for one swap quote. Execution and preview
/// construct the same context after validating their instruction accounts and
/// reading `Clock` once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SwapRequest {
    pub current_slot: u64,
    pub current_unix_timestamp: i64,
    pub asset_in: MarketAsset,
    pub reserve_credit: u64,
}

pub(crate) fn enforce_launch_same_transaction_guard(
    market: &Market,
    market_key: Pubkey,
    asset_in: MarketAsset,
    unix_timestamp: i64,
    instructions_sysvar: &AccountInfo<'_>,
) -> Result<()> {
    if !market
        .config
        .launch_rate_limit_active_for_swap(asset_in, unix_timestamp)
    {
        return Ok(());
    }
    require_keys_eq!(
        *instructions_sysvar.key,
        INSTRUCTIONS_SYSVAR_ID,
        ErrorCode::LaunchRateLimitSplitTransaction
    );
    let current_index = usize::from(
        load_current_index_checked(instructions_sysvar).map_err(|_| ErrorCode::LaunchRateLimitSplitTransaction)?,
    );
    let current = load_instruction_at_checked(current_index, instructions_sysvar)
        .map_err(|_| ErrorCode::LaunchRateLimitSplitTransaction)?;
    require!(
        current.program_id == crate::ID
            && launch_price_moving_instruction(&current.data)
            && current.accounts.iter().any(|meta| meta.pubkey == market_key),
        ErrorCode::LaunchRateLimitSplitTransaction
    );

    let mut matching_market_actions = 0_u8;
    let mut index = 0_usize;
    while let Ok(instruction) = load_instruction_at_checked(index, instructions_sysvar) {
        if instruction.program_id == crate::ID
            && launch_price_moving_instruction(&instruction.data)
            && instruction.accounts.iter().any(|meta| meta.pubkey == market_key)
        {
            matching_market_actions = matching_market_actions
                .checked_add(1)
                .ok_or(ErrorCode::LaunchRateLimitSplitTransaction)?;
            require!(matching_market_actions <= 1, ErrorCode::LaunchRateLimitSplitTransaction);
        }
        index = index.checked_add(1).ok_or(ErrorCode::LaunchRateLimitSplitTransaction)?;
    }
    require_eq!(matching_market_actions, 1, ErrorCode::LaunchRateLimitSplitTransaction);
    Ok(())
}

fn launch_price_moving_instruction(data: &[u8]) -> bool {
    let Some(discriminator) = data.get(..8) else {
        return false;
    };
    discriminator == crate::instruction::Swap::DISCRIMINATOR
        || discriminator == crate::instruction::OpenLeverage::DISCRIMINATOR
        || discriminator == crate::instruction::IncreaseLeverage::DISCRIMINATOR
        || discriminator == crate::instruction::DecreaseLeverage::DISCRIMINATOR
        || discriminator == crate::instruction::CloseLeverage::DISCRIMINATOR
        || discriminator == crate::instruction::LiquidateLeverage::DISCRIMINATOR
}

/// State-only preparation shared by preview and execution. `finalize_state`
/// commits the matching state transition; token settlement remains an
/// instruction concern.
pub(crate) struct PreparedSwap {
    pub quote: AmmSwapQuote,
    pub base_pre_rebalance: HlpRebalanceReceipt,
    pub quote_pre_rebalance: HlpRebalanceReceipt,
    pub fee_eligible_ylp_supply: u64,
    pub interest_eligibility: HlpYieldEligibility,
    pub cash_policy: SwapCashPolicy,
    pub(crate) explicit_transition: Option<Box<ExplicitHlpTransition>>,
}

impl core::fmt::Debug for PreparedSwap {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PreparedSwap")
            .field("quote", &self.quote)
            .field("base_pre_rebalance", &self.base_pre_rebalance)
            .field("quote_pre_rebalance", &self.quote_pre_rebalance)
            .field("fee_eligible_ylp_supply", &self.fee_eligible_ylp_supply)
            .field("interest_eligibility", &self.interest_eligibility)
            .field("cash_policy", &self.cash_policy)
            .field("has_explicit_transition", &self.explicit_transition.is_some())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FinalizedSwapState {
    pub base_rebalance: HlpRebalanceReceipt,
    pub quote_rebalance: HlpRebalanceReceipt,
}

impl PreparedSwap {
    /// Commits every state-only consequence of an already prepared swap.
    /// Spot execution and preview share this path; token transfers, yLP mint/
    /// burn CPIs, and reserve-custody checks remain instruction concerns.
    pub(crate) fn finalize_state(
        &self,
        market: &mut Market,
        current_slot: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<FinalizedSwapState> {
        require!(self.cash_policy == SwapCashPolicy::Spot, ErrorCode::BrokenInvariant);
        let explicit_transition = self.explicit_transition.as_deref().ok_or(ErrorCode::BrokenInvariant)?;
        self.finalize_explicit_state(
            market,
            current_slot,
            protocol_fee_bps,
            protocol_auction_split,
            explicit_transition,
        )
    }

    fn finalize_explicit_state(
        &self,
        market: &mut Market,
        current_slot: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        transition: &ExplicitHlpTransition,
    ) -> Result<FinalizedSwapState> {
        let quote = self.quote;
        let fee_asset = MarketAsset::try_from_code(quote.fee.fee_asset)?;
        let (base_fee_credit, distributed_surcharge_credit) =
            split_claimable_fee_credit(&quote.fee, quote.fee.claimable_fee_debit)?;
        if fee_asset == quote.asset_in {
            require_eq!(quote.amount_out, quote.gross_amount_out, ErrorCode::BrokenInvariant);
            require_eq!(
                quote.fee.reserve_input_credit,
                quote
                    .fee
                    .amount_in_for_quote
                    .checked_add(quote.fee.retained_surcharge)
                    .ok_or(ErrorCode::ReserveOverflow)?,
                ErrorCode::BrokenInvariant
            );
        } else {
            require!(fee_asset == quote.asset_in.opposite(), ErrorCode::BrokenInvariant);
            require_eq!(
                quote.fee.amount_in_for_quote,
                quote.fee.reserve_credit,
                ErrorCode::BrokenInvariant
            );
            require_eq!(
                quote.fee.reserve_input_credit,
                quote.fee.reserve_credit,
                ErrorCode::BrokenInvariant
            );
            require_eq!(
                quote
                    .amount_out
                    .checked_add(quote.fee.claimable_fee_debit)
                    .and_then(|value| value.checked_add(quote.fee.retained_surcharge))
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                quote.gross_amount_out,
                ErrorCode::BrokenInvariant
            );
        }
        {
            let (side_in, side_out) = market.swap_sides_mut(quote.asset_in);
            require_gte!(
                side_out.reserves.cash_reserve,
                quote.gross_amount_out,
                ErrorCode::InsufficientLiquidity
            );
            side_in.credit_reserve(quote.fee.amount_in_for_quote, true)?;
            side_out.debit_reserve(quote.gross_amount_out, true)?;
        }

        // Fee ownership is frozen before hLP-owned yLP is reconstructed.
        {
            market.side_mut(fee_asset).record_claimable_swap_fees(
                base_fee_credit,
                distributed_surcharge_credit,
                protocol_fee_bps,
                protocol_auction_split,
                self.fee_eligible_ylp_supply,
            )?;
            market.base_side.assert_share_backing()?;
            market.quote_side.assert_share_backing()?;
            market.side(fee_asset).fees.assert_backed()?;
        }
        if quote.fee.retained_surcharge > 0 {
            market.credit_protected_recenter_reserve(fee_asset, quote.fee.retained_surcharge)?;
        }
        let (base_rebalance, quote_rebalance) = transition.consume(market)?;
        market.finalize_amm_trade_after_inventory_checkpoint(
            quote.start_price_nad,
            quote.reserve_end_price_nad,
            current_slot,
        )?;
        let q_nad = market
            .amm
            .explicit_curve_cache
            .tail_liquidity
            .checked_add(market.amm.explicit_curve_cache.concentrated_liquidity)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        // The quote already computed this price from the retained-principal
        // endpoint. The algebraic hLP transition changes only ownership and
        // matching debt around that same ordinary reserve point, so rebuilding
        // and revalidating the curve here would be redundant.
        let final_price_nad = quote.reserve_end_price_nad;
        require!(final_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        market.observe_risk_from_explicit_curve(final_price_nad, q_nad, current_slot)?;
        market.assert_market_invariants()?;
        Ok(FinalizedSwapState {
            base_rebalance,
            quote_rebalance,
        })
    }
}

impl SwapRequest {
    pub(crate) fn prepare(self, market: &mut Market) -> Result<PreparedSwap> {
        self.prepare_with_cash_policy(market, SwapCashPolicy::Spot)
    }

    pub(crate) fn prepare_with_cash_policy(
        self,
        market: &mut Market,
        cash_policy: SwapCashPolicy,
    ) -> Result<PreparedSwap> {
        market.accrue_interest_to_slot(self.current_slot)?;
        require_eq!(
            market.base_side.shares.ylp_supply,
            market.quote_side.shares.ylp_supply,
            ErrorCode::BrokenInvariant
        );
        let interest_eligibility = HlpYieldEligibility {
            ylp_supply: market.base_side.shares.ylp_supply,
            base_hlp_ylp_shares: market.base_hlp_vault.ylp_shares,
            quote_hlp_ylp_shares: market.quote_hlp_vault.ylp_shares,
        };
        if market.base_side.reserves.live_reserve > 0 && market.quote_side.reserves.live_reserve > 0 {
            market.prepare_amm_for_swap(self.current_slot)?;
        }

        // Explicit tail+band markets quote and hedge in one algebraic path.
        // Apply at most one center target derived from an earlier observation
        // before freezing this swap's fee/curve state. The observation made by
        // this swap can only schedule a target for a later operation.
        market.config.amm.explicit_curve_parameters()?;
        market.advance_one_amm_controller_target(self.current_slot)?;
        let pre_state = market.dynamic_fee_pre_state(self.current_slot)?;
        let preliminary = market.preliminary_swap_inputs_for_state_at_time(
            self.asset_in,
            self.reserve_credit,
            self.current_slot,
            self.current_unix_timestamp,
            pre_state,
        )?;
        let integrated_start = market.integrated_curve_state_nad()?;
        let mut explicit = market
            .quote_explicit_integrated_with_fee_from_state(
                self.asset_in,
                self.reserve_credit,
                preliminary,
                integrated_start,
            )?
            .ok_or(ErrorCode::BrokenInvariant)?;
        if cash_policy == SwapCashPolicy::Spot {
            crate::market::liquidity::apply_explicit_hlp_recovery(
                market,
                self.asset_in,
                integrated_start,
                &mut explicit,
            )?;
        }
        let transition = prepare_explicit_hlp_transition(market, explicit, self.asset_in)?;
        require!(
            transition
                .interest_cash_floors(self.asset_in, explicit.gross_amount_out)
                .available(market),
            ErrorCode::InsufficientLiquidity
        );
        let quote = explicit.as_swap_quote(self.asset_in);
        Ok(PreparedSwap {
            quote,
            base_pre_rebalance: HlpRebalanceReceipt {
                target_asset: MarketAsset::Base,
                ..HlpRebalanceReceipt::default()
            },
            quote_pre_rebalance: HlpRebalanceReceipt {
                target_asset: MarketAsset::Quote,
                ..HlpRebalanceReceipt::default()
            },
            fee_eligible_ylp_supply: market
                .side(MarketAsset::try_from_code(quote.fee.fee_asset)?)
                .shares
                .ylp_supply,
            interest_eligibility,
            cash_policy,
            explicit_transition: Some(Box::new(transition)),
        })
    }
}

pub(crate) fn hlp_receipt_mutates_curve_inventory(receipt: &HlpRebalanceReceipt) -> bool {
    receipt.executed_delta != 0
        || receipt.ylp_mint_amount != 0
        || receipt.ylp_burn_amount != 0
        || receipt.debt_delta != 0
        || receipt.interest_paid != 0
}

pub(crate) fn rebalance_executes_token_changes(receipt: &HlpRebalanceReceipt) -> bool {
    receipt.ylp_mint_amount > 0 || receipt.ylp_burn_amount > 0 || receipt.interest_paid > 0
}

pub(crate) fn split_claimable_fee_credit(fee: &SwapFeeBreakdown, total_credit: u64) -> Result<(u64, u64)> {
    require_eq!(
        fee.base_fee_debit
            .checked_add(fee.distributed_surcharge_debit)
            .ok_or(ErrorCode::FeeMathOverflow)?,
        fee.claimable_fee_debit,
        ErrorCode::BrokenInvariant
    );
    require_gte!(fee.claimable_fee_debit, total_credit, ErrorCode::BrokenInvariant);
    if fee.claimable_fee_debit == 0 {
        require_eq!(total_credit, 0, ErrorCode::BrokenInvariant);
        return Ok((0, 0));
    }
    let base_credit = u64::try_from(
        (total_credit as u128)
            .checked_mul(fee.base_fee_debit as u128)
            .and_then(|value| value.checked_div(fee.claimable_fee_debit as u128))
            .ok_or(ErrorCode::FeeMathOverflow)?,
    )
    .map_err(|_| ErrorCode::FeeMathOverflow)?;
    Ok((
        base_credit,
        total_credit
            .checked_sub(base_credit)
            .ok_or(ErrorCode::FeeMathOverflow)?,
    ))
}

#[cfg(test)]
mod launch_guard_tests {
    use super::*;
    use anchor_lang::solana_program::sysvar::{
        self,
        instructions::{construct_instructions_data, store_current_index, BorrowedAccountMeta, BorrowedInstruction},
    };

    fn guarded_market() -> Market {
        let mut market = Market::default();
        market.config.start_time = 100;
        market.config.amm.launch_rate_limit_asset = crate::state::LAUNCH_RATE_LIMIT_ASSET_BASE;
        market.config.amm.launch_rate_limit_duration_seconds = 100;
        market
    }

    fn run_guard(
        market: &Market,
        market_key: Pubkey,
        instructions: &[(Pubkey, Pubkey, &'static [u8])],
        current_index: u16,
        unix_timestamp: i64,
    ) -> Result<()> {
        let borrowed: Vec<_> = instructions
            .iter()
            .map(|(program_id, account_key, data)| BorrowedInstruction {
                program_id,
                accounts: vec![BorrowedAccountMeta {
                    pubkey: account_key,
                    is_signer: false,
                    is_writable: true,
                }],
                data,
            })
            .collect();
        let mut data = construct_instructions_data(&borrowed);
        store_current_index(&mut data, current_index);
        let mut lamports = 0;
        let key = INSTRUCTIONS_SYSVAR_ID;
        let owner = sysvar::id();
        let account = AccountInfo::new(&key, false, false, &mut lamports, &mut data, &owner, false, 0);
        enforce_launch_same_transaction_guard(market, market_key, MarketAsset::Quote, unix_timestamp, &account)
    }

    #[test]
    fn launch_guard_allows_one_market_action_and_rejects_same_transaction_splits() {
        let market = guarded_market();
        let market_key = Pubkey::new_unique();
        let swap = crate::instruction::Swap::DISCRIMINATOR;
        assert!(run_guard(&market, market_key, &[(crate::ID, market_key, swap)], 0, 100).is_ok());
        assert!(run_guard(
            &market,
            market_key,
            &[(crate::ID, market_key, &[]), (crate::ID, market_key, swap)],
            1,
            100,
        )
        .is_ok());
        assert!(run_guard(
            &market,
            market_key,
            &[(crate::ID, market_key, swap), (crate::ID, market_key, swap)],
            0,
            100,
        )
        .is_err());
    }

    #[test]
    fn launch_guard_rejects_aggregator_cpi_but_is_inert_after_expiry() {
        let market = guarded_market();
        let market_key = Pubkey::new_unique();
        let aggregator = Pubkey::new_unique();
        let swap = crate::instruction::Swap::DISCRIMINATOR;
        assert!(run_guard(&market, market_key, &[(aggregator, market_key, swap)], 0, 100).is_err());
        assert!(run_guard(
            &market,
            market_key,
            &[(crate::ID, market_key, swap), (crate::ID, market_key, swap)],
            0,
            200,
        )
        .is_ok());
    }
}
