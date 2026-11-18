#[cfg(all(feature = "legacy-retirement", not(feature = "production")))]
compile_error!("Legacy retirement requires the explicit production profile.");
#[cfg(all(
    feature = "legacy-retirement",
    any(feature = "benchmark", feature = "devnet-short-notice")
))]
compile_error!("Legacy retirement cannot enable benchmark or devnet notice behavior.");

#[cfg(all(feature = "production", feature = "devnet-short-notice"))]
compile_error!("The devnet notice test cannot be enabled in a production build.");

#[cfg(not(feature = "legacy-retirement"))]
use crate::errors::ErrorCode;
use anchor_lang::prelude::*;

#[cfg(not(feature = "legacy-retirement"))]
pub mod account;
#[cfg(all(feature = "benchmark", not(feature = "legacy-retirement")))]
/// Off-chain-only access to Dusk's native market transitions.
///
/// This module is available only with the non-default `benchmark` feature. It
/// wraps the same integer state machines used by instruction preview and
/// execution; it does not provide an alternate economic model.
pub mod benchmark_api {
    include!("../benchmark/benchmark_api.rs");
}
#[cfg(not(feature = "legacy-retirement"))]
pub mod constants;
#[cfg(not(feature = "legacy-retirement"))]
pub mod errors;
#[cfg(not(feature = "legacy-retirement"))]
pub mod events;
// The program's own allocator; consumers that embed Dusk keep theirs.
#[cfg(all(feature = "custom-heap", not(feature = "no-entrypoint")))]
mod heap;
#[cfg(not(feature = "legacy-retirement"))]
pub mod instructions;
#[cfg(not(feature = "legacy-retirement"))]
pub mod math;
#[cfg(not(feature = "legacy-retirement"))]
pub mod oracle;
#[cfg(not(feature = "legacy-retirement"))]
pub mod state;
#[cfg(not(feature = "legacy-retirement"))]
pub mod token;
#[cfg(not(feature = "legacy-retirement"))]
pub mod transitions;

#[cfg(not(feature = "legacy-retirement"))]
pub use instructions::*;
#[cfg(not(feature = "legacy-retirement"))]
pub use state::*;
#[cfg(not(feature = "legacy-retirement"))]
use transitions::amm_liquidation::EmergencyLiquidationQuote;

#[cfg(not(feature = "no-entrypoint"))]
use solana_security_txt::security_txt;

#[cfg(not(feature = "no-entrypoint"))]
security_txt! {
    name: "Omnipair V2 (Dusk)",
    project_url: "https://omnipair.fi",
    contacts: "email:security@omnipair.fi,telegram:rustfully",
    source_code: "https://github.com/omnipair/dusk",
    source_release: env!("GIT_RELEASE"),
    source_revision: env!("GIT_REV"),
    auditors: "Pending final Dusk security review",
    policy: "https://omnipair.fi/security"
}

declare_id!("DuskWhswJYrzABDn8GqQD191X9RobRi5jhgGntXEdnKj");

/// Runs a handler that mutates the interest ledger, then checks the ledger
/// against the debt it measures (`Market::assert_interest_ledgers`), so a
/// missed settlement fails in the instruction that left it instead of being
/// written off as a loss by a later recognition.
#[cfg(not(feature = "legacy-retirement"))]
macro_rules! ledger_checked {
    ($ctx:ident, $($market:ident).+, $handler:expr) => {{
        let Context {
            program_id,
            accounts,
            remaining_accounts,
            bumps,
        } = $ctx;
        {
            let $ctx = Context::new(program_id, &mut *accounts, remaining_accounts, bumps);
            $handler?;
        }
        accounts.$($market).+.assert_interest_ledgers()
    }};
}

#[cfg(not(feature = "legacy-retirement"))]
#[program]
pub mod dusk {
    use super::*;

    pub fn update_lp_metadata(ctx: Context<UpdateLpMetadata>, args: InitializeLpMetadataArgs) -> Result<()> {
        UpdateLpMetadata::handle(ctx, args)
    }

    pub fn burn_retired_junior(ctx: Context<BurnRetiredJunior>, args: BurnRetiredJuniorArgs) -> Result<()> {
        BurnRetiredJunior::handle_burn(ctx, args)
    }

    pub fn fund_junior_seed<'info>(ctx: Context<'_, '_, '_, 'info, FundJuniorSeed<'info>>) -> Result<()> {
        FundJuniorSeed::handle_fund(ctx)
    }

    pub fn compound_junior_seed<'info>(ctx: Context<'_, '_, '_, 'info, CompoundJuniorSeed<'info>>) -> Result<()> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        CompoundJuniorSeed::handle_compound(ctx)
    }

    pub fn initialize_junior(ctx: Context<InitializeJunior>, args: InitializeJuniorArgs) -> Result<()> {
        InitializeJunior::handle_initialize(ctx, args)
    }

    pub fn convert_tranche<'info>(
        ctx: Context<'_, '_, '_, 'info, ConvertTranche<'info>>,
        action: TrancheAction,
    ) -> Result<()> {
        ConvertTranche::handle_convert(ctx, action)
    }

    // Futarchy authority instructions
    pub fn init_futarchy_authority(ctx: Context<InitFutarchyAuthority>, args: InitFutarchyAuthorityArgs) -> Result<()> {
        InitFutarchyAuthority::handle_init(ctx, args)
    }

    pub fn update_futarchy_authority(
        ctx: Context<UpdateFutarchyAuthority>,
        args: UpdateFutarchyAuthorityArgs,
    ) -> Result<()> {
        UpdateFutarchyAuthority::handle_update(ctx, args)
    }

    pub fn update_protocol_revenue(ctx: Context<UpdateProtocolRevenue>, args: UpdateProtocolRevenueArgs) -> Result<()> {
        UpdateProtocolRevenue::handle_update(ctx, args)
    }

    pub fn update_revenue_recipients(
        ctx: Context<UpdateRevenueRecipients>,
        args: UpdateRevenueRecipientsArgs,
    ) -> Result<()> {
        UpdateRevenueRecipients::handle_update(ctx, args)
    }

    pub fn update_protocol_auction_config(
        ctx: Context<UpdateProtocolAuctionConfig>,
        args: UpdateProtocolAuctionConfigArgs,
    ) -> Result<()> {
        UpdateProtocolAuctionConfig::handle_update(ctx, args)
    }

    pub fn update_protocol_auction_recipients(
        ctx: Context<UpdateProtocolAuctionRecipients>,
        args: UpdateProtocolAuctionRecipientsArgs,
    ) -> Result<()> {
        UpdateProtocolAuctionRecipients::handle_update(ctx, args)
    }

    pub fn configure_protocol_auction(
        ctx: Context<ConfigureProtocolAuction>,
        args: ConfigureProtocolAuctionArgs,
    ) -> Result<()> {
        ConfigureProtocolAuction::handle_configure(ctx, args)
    }

    pub fn set_global_reduce_only(ctx: Context<SetGlobalReduceOnly>, args: SetGlobalReduceOnlyArgs) -> Result<()> {
        SetGlobalReduceOnly::handle_set_global_reduce_only(ctx, args)
    }

    // Referral instructions
    pub fn configure_referral_partner(
        ctx: Context<ConfigureReferralPartner>,
        args: ConfigureReferralPartnerArgs,
    ) -> Result<()> {
        ConfigureReferralPartner::handle_configure(ctx, args)
    }

    pub fn initialize_referral_accrual(ctx: Context<InitializeReferralAccrual>) -> Result<()> {
        InitializeReferralAccrual::handle_initialize(ctx)
    }

    pub fn set_referral_recipient(ctx: Context<SetReferralRecipient>, args: SetReferralRecipientArgs) -> Result<()> {
        SetReferralRecipient::handle_set(ctx, args)
    }

    #[access_control(ctx.accounts.validate())]
    pub fn claim_referral_interest<'info>(ctx: Context<'_, '_, '_, 'info, ClaimReferralInterest<'info>>) -> Result<()> {
        ledger_checked!(ctx, market, ClaimReferralInterest::handle_claim(ctx))
    }

    // Protocol auction instructions
    pub fn settle_protocol_auction<'info>(
        ctx: Context<'_, '_, '_, 'info, SettleProtocolAuction<'info>>,
        args: SettleProtocolAuctionArgs,
    ) -> Result<()> {
        ctx.accounts.book_pending_interest()?;
        ctx.accounts.validate(&args)?;
        ledger_checked!(ctx, market, SettleProtocolAuction::handle_settle(ctx, args))
    }

    // Market instructions
    #[access_control(ctx.accounts.validate(&args))]
    pub fn initialize_market(ctx: Context<InitializeMarket>, args: InitializeMarketArgs) -> Result<()> {
        InitializeMarket::handle_initialize(ctx, args)
    }

    #[access_control(ctx.accounts.validate())]
    pub fn set_market_reduce_only(ctx: Context<SetMarketReduceOnly>, args: SetMarketReduceOnlyArgs) -> Result<()> {
        SetMarketReduceOnly::handle_set(ctx, args)
    }

    /// Announce one atomic change. An optional manager may propose parameters only; LP balances grant no authority.
    pub fn announce_operator_change(
        ctx: Context<AnnounceOperatorChange>,
        args: AnnounceOperatorChangeArgs,
    ) -> Result<()> {
        AnnounceOperatorChange::handle(ctx, args)
    }

    /// Permissionless execution after the full exit notice. A new authority accepts a handoff by signing.
    pub fn execute_operator_change<'info>(ctx: Context<'_, '_, '_, 'info, ExecuteOperatorChange<'info>>) -> Result<()> {
        ledger_checked!(ctx, market, ExecuteOperatorChange::handle(ctx))
    }

    /// The operator may cancel any bundle; a manager may cancel its own. Anyone may close obsolete proposals.
    pub fn resolve_operator_change(ctx: Context<ResolveOperatorChange>, cancel: bool) -> Result<()> {
        ResolveOperatorChange::handle(ctx, cancel)
    }

    /// Immediately revoke the optional manager and its pending change, preserving owner announcements.
    pub fn remove_market_manager(ctx: Context<RemoveMarketManager>) -> Result<()> {
        RemoveMarketManager::handle(ctx)
    }

    /// Prepare a recipient archive with a separate funded payer.
    pub fn initialize_operator_revenue(ctx: Context<InitializeOperatorRevenue>) -> Result<()> {
        InitializeOperatorRevenue::handle(ctx)
    }
    /// Permissionlessly pay earned interest fees to the immutable recipient ATA.
    pub fn claim_operator_revenue<'info>(
        ctx: Context<'_, '_, '_, 'info, ClaimOperatorRevenue<'info>>,
        args: ClaimOperatorRevenueArgs,
    ) -> Result<()> {
        ledger_checked!(ctx, market, ClaimOperatorRevenue::handle(ctx, args))
    }

    // Liquidity instructions
    pub fn add_liquidity<'info>(
        ctx: Context<'_, '_, '_, 'info, AddLiquidity<'info>>,
        args: AddLiquidityArgs,
    ) -> Result<()> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        ctx.accounts.update_and_validate(&args)?;
        ledger_checked!(ctx, market, AddLiquidity::handle_add_liquidity(ctx, args))
    }

    /// Permissionless launch adapters use the same fully-backed seeding path
    /// as ordinary srLP deposits. The initializer is the one-shot authority;
    /// after this succeeds, subsequent liquidity is permissionless.
    pub fn open_liquidity_gates<'info>(
        ctx: Context<'_, '_, '_, 'info, AddLiquidity<'info>>,
        args: AddLiquidityArgs,
    ) -> Result<()> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        ctx.accounts.update_and_validate(&args)?;
        require_eq!(
            ctx.accounts.market.base_side.shares.srlp_supply,
            0,
            ErrorCode::NonZeroSupply
        );
        ledger_checked!(ctx, market, AddLiquidity::handle_add_liquidity(ctx, args))
    }

    pub fn remove_liquidity<'info>(
        ctx: Context<'_, '_, '_, 'info, RemoveLiquidity<'info>>,
        args: RemoveLiquidityArgs,
    ) -> Result<()> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        ctx.accounts.update_and_validate(&args)?;
        ledger_checked!(ctx, market, RemoveLiquidity::handle_remove_liquidity(ctx, args))
    }

    #[access_control(ctx.accounts.validate(&args))]
    pub fn set_yield_recipient(ctx: Context<SetYieldRecipient>, args: SetYieldRecipientArgs) -> Result<()> {
        SetYieldRecipient::handle_set(ctx, args)
    }

    /// Set or revoke a harvest caller independently of the yield recipient.
    /// Only the LP owner may update this authority; `None` revokes it.
    #[access_control(ctx.accounts.validate(&args))]
    pub fn set_harvest_authority(ctx: Context<SetHarvestAuthority>, args: SetHarvestAuthorityArgs) -> Result<()> {
        SetHarvestAuthority::handle_set(ctx, args)
    }

    pub fn harvest<'info>(ctx: Context<'_, '_, '_, 'info, Harvest<'info>>, args: HarvestArgs) -> Result<()> {
        // Harvest pays income that has already accrued and prices nothing,
        // so an oracle outage must not hold it. A supplied feed still refreshes
        // the market as before.
        ctx.accounts
            .market
            .bind_optional_repayment_oracle(ctx.remaining_accounts, &Clock::get()?);
        ctx.accounts.update_and_validate(&args)?;
        ledger_checked!(ctx, market, Harvest::handle_harvest(ctx, args))
    }

    #[access_control(ctx.accounts.validate(&args))]
    pub fn initialize_yield_accounts<'info>(
        ctx: Context<'_, '_, '_, 'info, InitializeYieldAccounts<'info>>,
        args: InitializeYieldAccountsArgs,
    ) -> Result<()> {
        ledger_checked!(ctx, market, InitializeYieldAccounts::handle_initialize(ctx, args))
    }

    #[access_control(ctx.accounts.validate(ctx.remaining_accounts))]
    pub fn initialize_lp_transfer_hook<'info>(
        ctx: Context<'_, '_, '_, 'info, InitializeLpTransferHook<'info>>,
    ) -> Result<()> {
        InitializeLpTransferHook::handle_initialize(ctx)
    }

    // Spot instructions
    pub fn swap<'info>(ctx: Context<'_, '_, '_, 'info, Swap<'info>>, args: SwapArgs) -> Result<()> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        ledger_checked!(ctx, market, Swap::handle_swap(ctx, args))
    }

    /// Quote the fixed net payment for vault-wide atomic external hLP recovery.
    pub fn preview_hlp_flash(
        ctx: Context<PreviewHlpFlash>,
        target_asset: u8,
    ) -> Result<transitions::liquidity::HlpFlashQuote> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        PreviewHlpFlash::handle(ctx, target_asset)
    }

    /// Release hLP target collateral, bound to an exact top-level settlement
    /// later in this same transaction. No routing program is privileged.
    pub fn begin_hlp_flash<'info>(
        ctx: Context<'_, '_, '_, 'info, BeginHlpFlash<'info>>,
        args: BeginHlpFlashArgs,
    ) -> Result<()> {
        ctx.accounts
            .accounts
            .market
            .bind_current_oracle(ctx.remaining_accounts)?;
        ledger_checked!(ctx, accounts.market, BeginHlpFlash::handle(ctx, args))
    }

    /// Verify the fixed payment and restore the vault's opposite-token hedge.
    pub fn settle_hlp_flash<'info>(ctx: Context<'_, '_, '_, 'info, SettleHlpFlash<'info>>) -> Result<()> {
        ctx.accounts
            .accounts
            .market
            .bind_current_oracle(ctx.remaining_accounts)?;
        ledger_checked!(ctx, accounts.market, SettleHlpFlash::handle(ctx))
    }

    /// Permissionlessly closes an hLP whose current-slot symmetric-EMA backing
    /// cannot cover funding debt. Native recovery uses EMA price, smoothed depth,
    /// concentration and configured fees. Recovery protects principal first;
    /// junior absorbs principal loss before senior; unpaid interest is canceled.
    /// Interest the vault held while frozen counts as recovery. The bounded
    /// caller reward and the protocol share of the funding paid take only what
    /// borrowed-asset cash allows. Existing funded holder claims remain
    /// claimable after cleanup.
    pub fn close_insolvent_hlp<'info>(
        ctx: Context<'_, '_, '_, 'info, CloseInsolventHlp<'info>>,
        args: CloseInsolventHlpArgs,
    ) -> Result<()> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        let current_slot = ctx.accounts.update_and_validate(&args)?;
        ledger_checked!(ctx, market, CloseInsolventHlp::handle(ctx, args, current_slot))
    }

    pub fn begin_flash_liquidation<'info>(
        ctx: Context<'_, '_, '_, 'info, BeginFlashLiquidation<'info>>,
        args: BeginFlashLiquidationArgs,
    ) -> Result<()> {
        ctx.accounts
            .accounts
            .market
            .bind_current_oracle(ctx.remaining_accounts)?;
        ledger_checked!(ctx, accounts.market, BeginFlashLiquidation::handle(ctx, args))
    }

    pub fn preview_flash_liquidation(
        ctx: Context<PreviewFlashLiquidation>,
        args: PreviewFlashLiquidationArgs,
    ) -> Result<FlashLiquidationQuote> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        PreviewFlashLiquidation::handle(ctx, args)
    }

    /// Preview the final debt-token payout when the paired Dusk AMM funds an
    /// atomic flash liquidation settlement.
    pub fn preview_flash_amm_liquidation(
        ctx: Context<PreviewEmergencyLiquidation>,
        args: PreviewFlashAmmLiquidationArgs,
    ) -> Result<FlashAmmLiquidationQuote> {
        ctx.accounts
            .preview
            .market
            .bind_current_oracle(ctx.remaining_accounts)?;
        PreviewEmergencyLiquidation::handle_flash_amm(ctx, args)
    }

    pub fn emergency_liquidation<'info>(
        ctx: Context<'_, '_, '_, 'info, EmergencyLiquidation<'info>>,
        args: EmergencyLiquidationArgs,
    ) -> Result<()> {
        ctx.accounts
            .accounts
            .market
            .bind_current_oracle(ctx.remaining_accounts)?;
        ledger_checked!(ctx, accounts.market, EmergencyLiquidation::handle(ctx, args))
    }

    pub fn preview_emergency_liquidation(
        ctx: Context<PreviewEmergencyLiquidation>,
        args: PreviewEmergencyLiquidationArgs,
    ) -> Result<EmergencyLiquidationQuote> {
        ctx.accounts
            .preview
            .market
            .bind_current_oracle(ctx.remaining_accounts)?;
        PreviewEmergencyLiquidation::handle(ctx, args)
    }

    /// Read current liquidation health without starting an episode or mutating accounts.
    pub fn preview_liquidation_health(
        ctx: Context<PreviewFlashLiquidation>,
        debt_asset: u8,
    ) -> Result<LiquidationHealth> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        PreviewFlashLiquidation::handle_health(ctx, debt_asset)
    }

    pub fn settle_flash_liquidation<'info>(
        ctx: Context<'_, '_, '_, 'info, SettleFlashLiquidation<'info>>,
    ) -> Result<()> {
        ctx.accounts
            .accounts
            .market
            .bind_current_oracle(ctx.remaining_accounts)?;
        ledger_checked!(ctx, accounts.market, SettleFlashLiquidation::handle(ctx))
    }

    // Lending instructions
    pub fn deposit_collateral<'info>(
        ctx: Context<'_, '_, '_, 'info, DepositCollateral<'info>>,
        args: DepositCollateralArgs,
    ) -> Result<()> {
        ctx.accounts
            .market
            .bind_optional_repayment_oracle(ctx.remaining_accounts, &Clock::get()?);
        ctx.accounts.update_and_validate(&args)?;
        ledger_checked!(ctx, market, DepositCollateral::handle_deposit(ctx, args))
    }

    pub fn withdraw_collateral<'info>(
        ctx: Context<'_, '_, '_, 'info, WithdrawCollateral<'info>>,
        args: WithdrawCollateralArgs,
    ) -> Result<()> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        ctx.accounts.update_and_validate(&args)?;
        ledger_checked!(ctx, market, WithdrawCollateral::handle_withdraw(ctx, args))
    }

    pub fn borrow<'info>(ctx: Context<'_, '_, '_, 'info, Borrow<'info>>, args: BorrowArgs) -> Result<()> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        ctx.accounts.update_and_validate(&args)?;
        ledger_checked!(ctx, market, Borrow::handle_borrow(ctx, args))
    }

    pub fn repay<'info>(ctx: Context<'_, '_, '_, 'info, Repay<'info>>, args: RepayArgs) -> Result<()> {
        ctx.accounts
            .market
            .bind_optional_repayment_oracle(ctx.remaining_accounts, &Clock::get()?);
        ctx.accounts.update_and_validate(&args)?;
        ledger_checked!(ctx, market, Repay::handle_repay(ctx, args))
    }

    /// Adds the signer's tokens to an existing borrow position without granting
    /// the donor ownership or withdrawal rights. The recorded owner is unchanged.
    pub fn donate_collateral<'info>(
        ctx: Context<'_, '_, '_, 'info, DonateCollateral<'info>>,
        args: DonateCollateralArgs,
    ) -> Result<()> {
        ctx.accounts
            .market
            .bind_optional_repayment_oracle(ctx.remaining_accounts, &Clock::get()?);
        ctx.accounts.update_and_validate(&args)?;
        ledger_checked!(ctx, market, DonateCollateral::handle_deposit(ctx, args))
    }

    /// Sweeps both collateral balances and closes a debt-free borrow position.
    /// Balances are read during execution so prior dust donations cannot block closure.
    pub fn withdraw_all_collateral<'info>(
        ctx: Context<'_, '_, '_, 'info, WithdrawAllCollateral<'info>>,
        args: WithdrawAllCollateralArgs,
    ) -> Result<()> {
        WithdrawAllCollateral::handle(ctx, args)
    }

    /// Returns all remaining collateral to its owner after full leverage repayment.
    pub fn withdraw_repaid_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, WithdrawRepaidLeverage<'info>>,
        args: WithdrawRepaidLeverageArgs,
    ) -> Result<()> {
        WithdrawRepaidLeverage::handle(ctx, args)
    }

    // Leverage instructions
    pub fn open_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, OpenLeverage<'info>>,
        args: OpenLeverageArgs,
    ) -> Result<()> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        let clock = Clock::get()?;
        ctx.accounts.validate_at(&args, clock.unix_timestamp)?;
        ledger_checked!(
            ctx,
            market,
            OpenLeverage::handle_open(ctx, args, clock.slot, clock.epoch, clock.unix_timestamp)
        )
    }

    pub fn close_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, CloseLeverage<'info>>,
        args: CloseLeverageArgs,
    ) -> Result<()> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        let clock = Clock::get()?;
        ctx.accounts.validate_at(&args, clock.unix_timestamp)?;
        ledger_checked!(
            ctx,
            market,
            CloseLeverage::handle_close(ctx, args, clock.slot, clock.epoch, clock.unix_timestamp)
        )
    }

    pub fn delegated_close_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, CloseLeverage<'info>>,
        args: DelegatedCloseLeverageArgs,
    ) -> Result<()> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        let clock = Clock::get()?;
        ctx.accounts.validate_delegated_at(&args, clock.unix_timestamp)?;
        ledger_checked!(
            ctx,
            market,
            CloseLeverage::handle_delegated_close(ctx, args, clock.slot, clock.epoch, clock.unix_timestamp)
        )
    }

    pub fn increase_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, IncreaseLeverage<'info>>,
        args: IncreaseLeverageArgs,
    ) -> Result<()> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        let clock = Clock::get()?;
        ctx.accounts.validate_at(&args, clock.unix_timestamp)?;
        ledger_checked!(
            ctx,
            market,
            IncreaseLeverage::handle_increase(ctx, args, clock.slot, clock.epoch, clock.unix_timestamp)
        )
    }

    pub fn decrease_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, DecreaseLeverage<'info>>,
        args: DecreaseLeverageArgs,
    ) -> Result<()> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        let clock = Clock::get()?;
        ctx.accounts.validate_at(&args, clock.unix_timestamp)?;
        ledger_checked!(
            ctx,
            market,
            DecreaseLeverage::handle_decrease(ctx, args, clock.slot, clock.unix_timestamp)
        )
    }

    pub fn add_leverage_margin<'info>(
        ctx: Context<'_, '_, '_, 'info, AddLeverageMargin<'info>>,
        args: AddLeverageMarginArgs,
    ) -> Result<()> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        let clock = Clock::get()?;
        ctx.accounts.validate_at(&args, clock.unix_timestamp)?;
        ledger_checked!(
            ctx,
            market,
            AddLeverageMargin::handle_add_margin(ctx, args, clock.slot, clock.epoch, true)
        )
    }

    /// Repays leverage from the signer's tokens without quoting or selling collateral.
    /// The update event's closeout_value is zero (unquoted).
    pub fn repay_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, AddLeverageMargin<'info>>,
        args: AddLeverageMarginArgs,
    ) -> Result<()> {
        let clock = Clock::get()?;
        ctx.accounts
            .market
            .bind_optional_repayment_oracle(ctx.remaining_accounts, &clock);
        ctx.accounts.validate_at(&args, clock.unix_timestamp)?;
        ledger_checked!(
            ctx,
            market,
            AddLeverageMargin::handle_add_margin(ctx, args, clock.slot, clock.epoch, false)
        )
    }

    #[access_control(ctx.accounts.validate(&args))]
    pub fn create_leverage_delegation(
        ctx: Context<CreateLeverageDelegation>,
        args: CreateLeverageDelegationArgs,
    ) -> Result<()> {
        CreateLeverageDelegation::handle_create(ctx, args)
    }

    #[access_control(ctx.accounts.validate(&args))]
    pub fn update_leverage_delegation(
        ctx: Context<UpdateLeverageDelegation>,
        args: UpdateLeverageDelegationArgs,
    ) -> Result<()> {
        UpdateLeverageDelegation::handle_update(ctx, args)
    }

    pub fn close_leverage_delegation(
        ctx: Context<CloseLeverageDelegation>,
        args: CloseLeverageDelegationArgs,
    ) -> Result<()> {
        CloseLeverageDelegation::handle_close(ctx, args)
    }

    // Preview instructions
    pub fn preview_hlp_deposit_capacity(ctx: Context<PreviewHlpDepositCapacity>) -> Result<HlpDepositCapacityPreview> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        PreviewHlpDepositCapacity::handle_preview(ctx)
    }

    pub fn preview_market(ctx: Context<PreviewMarket>) -> Result<MarketPreview> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        PreviewMarket::handle_preview(ctx)
    }

    pub fn preview_hlp_order_trigger(
        ctx: Context<PreviewHlpOrderTrigger>,
        args: PreviewHlpOrderTriggerArgs,
    ) -> Result<HlpOrderTriggerPreview> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        PreviewHlpOrderTrigger::handle_preview(ctx, args)
    }

    pub fn preview_add_liquidity(
        ctx: Context<PreviewAddLiquidity>,
        args: PreviewAddLiquidityArgs,
    ) -> Result<AddLiquidityPreview> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        PreviewAddLiquidity::handle_preview(ctx, args)
    }

    pub fn preview_swap(ctx: Context<PreviewSwap>, args: PreviewSwapArgs) -> Result<SwapPreview> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        PreviewSwap::handle_preview(ctx, args)
    }

    pub fn preview_borrow_capacity(
        ctx: Context<PreviewBorrowCapacity>,
        args: PreviewBorrowCapacityArgs,
    ) -> Result<BorrowCapacityPreview> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        PreviewBorrowCapacity::handle_preview(ctx, args)
    }

    pub fn preview_borrow_position_capacity(
        ctx: Context<PreviewBorrowPositionCapacity>,
        args: PreviewBorrowPositionCapacityArgs,
    ) -> Result<BorrowPositionCapacityPreview> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        PreviewBorrowPositionCapacity::handle_preview(ctx, args)
    }

    pub fn preview_borrow_position(ctx: Context<PreviewBorrowPosition>) -> Result<BorrowPositionPreview> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        PreviewBorrowPosition::handle_preview(ctx)
    }

    // HLP instructions
    pub fn rotate_hlp_cohort(ctx: Context<RotateHlpCohort>, args: RotateHlpCohortArgs) -> Result<()> {
        RotateHlpCohort::handle_rotate(ctx, args)
    }

    pub fn burn_retired_hlp_receipt(
        ctx: Context<BurnRetiredHlpReceipt>,
        args: BurnRetiredHlpReceiptArgs,
    ) -> Result<()> {
        BurnRetiredHlpReceipt::handle_burn(ctx, args)
    }

    pub fn deposit_single_sided<'info>(
        ctx: Context<'_, '_, '_, 'info, DepositSingleSided<'info>>,
        args: DepositSingleSidedArgs,
    ) -> Result<()> {
        ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        ctx.accounts.update_and_validate(&args)?;
        ledger_checked!(ctx, market, DepositSingleSided::handle_deposit(ctx, args))
    }

    pub fn withdraw_single_sided<'info>(
        ctx: Context<'_, '_, '_, 'info, WithdrawSingleSided<'info>>,
        args: WithdrawSingleSidedArgs,
    ) -> Result<()> {
        // Burning receipts of a terminally closed vault moves no reserves and
        // prices nothing, so it reads the oracle only when one is supplied.
        let terminal = ctx
            .accounts
            .market
            .asset_for_hlp_mint(ctx.accounts.target_hlp_mint.key())
            .is_ok_and(|asset| ctx.accounts.market.hlp_terminally_closed(asset));
        if terminal {
            ctx.accounts
                .market
                .bind_optional_repayment_oracle(ctx.remaining_accounts, &Clock::get()?);
        } else {
            ctx.accounts.market.bind_current_oracle(ctx.remaining_accounts)?;
        }
        ctx.accounts.update_and_validate(&args)?;
        if terminal {
            // The burn neither accrues nor touches debt, so it must not
            // depend on the rest of the market's ledger either.
            return WithdrawSingleSided::handle_withdraw(ctx, args);
        }
        ledger_checked!(ctx, market, WithdrawSingleSided::handle_withdraw(ctx, args))
    }

    pub fn fallback<'info>(program_id: &Pubkey, accounts: &'info [AccountInfo<'info>], data: &[u8]) -> Result<()> {
        crate::instructions::transfer_hook::handle_transfer_hook(program_id, accounts, data)
    }
}

#[cfg(feature = "legacy-retirement")]
pub mod retirement;
#[cfg(feature = "legacy-retirement")]
pub use retirement::*;

#[cfg(feature = "legacy-retirement")]
#[program]
pub mod dusk {
    use super::*;
    pub fn freeze_legacy_market<'info>(ctx: Context<'_, '_, '_, 'info, FreezeLegacyMarket<'info>>) -> Result<()> {
        retirement::handle_freeze(ctx)
    }
    pub fn abort_stale_legacy_freeze(ctx: Context<AbortStaleLegacyFreeze>) -> Result<()> {
        retirement::handle_abort_stale_freeze(ctx)
    }
    pub fn begin_legacy_retirement<'info>(
        ctx: Context<'_, '_, '_, 'info, BeginLegacyRetirement<'info>>,
        args: BeginLegacyRetirementArgs,
    ) -> Result<()> {
        retirement::handle_begin(ctx, args)
    }
    pub fn clear_legacy_owner<'info>(
        ctx: Context<'_, '_, '_, 'info, ClearLegacyOwner<'info>>,
        owner_index: u8,
    ) -> Result<()> {
        retirement::handle_clear(ctx, owner_index)
    }
    pub fn pay_legacy_owner(ctx: Context<PayLegacyRetirement>, owner_index: u8) -> Result<()> {
        retirement::handle_pay(ctx, owner_index)
    }
    pub fn finish_legacy_retirement(ctx: Context<PayLegacyRetirement>) -> Result<()> {
        retirement::handle_finish(ctx)
    }
}
