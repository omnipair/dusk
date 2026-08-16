use crate::errors::ErrorCode;
use anchor_lang::prelude::*;

pub mod account;
pub mod constants;
pub mod errors;
pub mod events;
pub mod instructions;
pub mod market;
pub mod math;
pub mod state;
pub mod token;

pub use instructions::*;
pub use state::*;

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

declare_id!("358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv");

#[program]
pub mod dusk {
    use super::*;

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

    pub fn update_protocol_auction_route(
        ctx: Context<UpdateProtocolAuctionRoute>,
        args: UpdateProtocolAuctionRouteArgs,
    ) -> Result<()> {
        UpdateProtocolAuctionRoute::handle_update(ctx, args)
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
        ClaimReferralInterest::handle_claim(ctx)
    }

    // Protocol auction instructions
    #[access_control(ctx.accounts.validate(&args))]
    pub fn settle_protocol_auction<'info>(
        ctx: Context<'_, '_, '_, 'info, SettleProtocolAuction<'info>>,
        args: SettleProtocolAuctionArgs,
    ) -> Result<()> {
        SettleProtocolAuction::handle_settle(ctx, args)
    }

    // Market instructions
    #[access_control(ctx.accounts.validate(&args))]
    pub fn initialize_market(ctx: Context<InitializeMarket>, args: InitializeMarketArgs) -> Result<()> {
        InitializeMarket::handle_initialize(ctx, args)
    }

    #[access_control(ctx.accounts.validate(&args))]
    pub fn initialize_lp_metadata(ctx: Context<InitializeLpMetadata>, args: InitializeLpMetadataArgs) -> Result<()> {
        InitializeLpMetadata::handle_initialize(ctx, args)
    }

    #[access_control(ctx.accounts.validate())]
    pub fn set_market_reduce_only(ctx: Context<SetMarketReduceOnly>, args: SetMarketReduceOnlyArgs) -> Result<()> {
        SetMarketReduceOnly::handle_set(ctx, args)
    }

    // Direct-yLP parameter governance instructions
    pub fn create_parameter_proposal<'info>(
        ctx: Context<'_, '_, '_, 'info, CreateParameterProposal<'info>>,
        args: CreateParameterProposalArgs,
    ) -> Result<()> {
        CreateParameterProposal::handle_create(ctx, args)
    }

    pub fn support_parameter_proposal<'info>(
        ctx: Context<'_, '_, '_, 'info, SupportParameterProposal<'info>>,
        args: SupportParameterProposalArgs,
    ) -> Result<()> {
        SupportParameterProposal::handle_support(ctx, args)
    }

    pub fn queue_parameter_proposal(ctx: Context<QueueParameterProposal>) -> Result<()> {
        QueueParameterProposal::handle_queue(ctx)
    }

    pub fn execute_parameter_proposal(ctx: Context<ExecuteParameterProposal>) -> Result<()> {
        ExecuteParameterProposal::handle_execute(ctx)
    }

    pub fn withdraw_parameter_support<'info>(
        ctx: Context<'_, '_, '_, 'info, WithdrawParameterSupport<'info>>,
    ) -> Result<()> {
        WithdrawParameterSupport::handle_withdraw(ctx)
    }

    // Liquidity instructions
    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn add_liquidity<'info>(
        ctx: Context<'_, '_, '_, 'info, AddLiquidity<'info>>,
        args: AddLiquidityArgs,
    ) -> Result<()> {
        AddLiquidity::handle_add_liquidity(ctx, args)
    }

    /// Permissionless launch adapters use the same fully-backed seeding path
    /// as ordinary yLP deposits. The initializer is the one-shot authority;
    /// after this succeeds, subsequent liquidity is permissionless.
    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn graduate_market<'info>(
        ctx: Context<'_, '_, '_, 'info, AddLiquidity<'info>>,
        args: AddLiquidityArgs,
    ) -> Result<()> {
        require_eq!(
            ctx.accounts.market.base_side.shares.ylp_supply,
            0,
            ErrorCode::NonZeroSupply
        );
        AddLiquidity::handle_add_liquidity(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn remove_liquidity<'info>(
        ctx: Context<'_, '_, '_, 'info, RemoveLiquidity<'info>>,
        args: RemoveLiquidityArgs,
    ) -> Result<()> {
        RemoveLiquidity::handle_remove_liquidity(ctx, args)
    }

    #[access_control(ctx.accounts.validate(&args))]
    pub fn set_yield_recipient(ctx: Context<SetYieldRecipient>, args: SetYieldRecipientArgs) -> Result<()> {
        SetYieldRecipient::handle_set(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn claim_yield<'info>(ctx: Context<'_, '_, '_, 'info, ClaimYield<'info>>, args: ClaimYieldArgs) -> Result<()> {
        ClaimYield::handle_claim(ctx, args)
    }

    #[access_control(ctx.accounts.validate(&args))]
    pub fn initialize_yield_accounts<'info>(
        ctx: Context<'_, '_, '_, 'info, InitializeYieldAccounts<'info>>,
        args: InitializeYieldAccountsArgs,
    ) -> Result<()> {
        InitializeYieldAccounts::handle_initialize(ctx, args)
    }

    #[access_control(ctx.accounts.validate())]
    pub fn initialize_lp_transfer_hook<'info>(
        ctx: Context<'_, '_, '_, 'info, InitializeLpTransferHook<'info>>,
    ) -> Result<()> {
        InitializeLpTransferHook::handle_initialize(ctx)
    }

    // Spot instructions
    pub fn swap<'info>(ctx: Context<'_, '_, '_, 'info, Swap<'info>>, args: SwapArgs) -> Result<()> {
        let mode = SwapExecutionMode::Ordinary;
        let (current_slot, current_epoch, current_unix_timestamp) =
            ctx.accounts.validate_and_read_clock(&args, mode)?;
        Swap::handle_swap(ctx, args, current_slot, current_epoch, current_unix_timestamp, mode)
    }

    /// Permissionless critical hLP recovery. The caller supplies the hLP's
    /// borrowed asset and receives target collateral through the same exact
    /// O(1) swap/hedge transition as an ordinary recovery swap. It is live in
    /// reduce-only mode and rejects unless the selected vault is at or beyond
    /// the 9/8 funding-stress boundary.
    pub fn liquidate_hlp<'info>(ctx: Context<'_, '_, '_, 'info, Swap<'info>>, args: SwapArgs) -> Result<()> {
        let mode = SwapExecutionMode::CriticalHlpLiquidation;
        let (current_slot, current_epoch, current_unix_timestamp) =
            ctx.accounts.validate_and_read_clock(&args, mode)?;
        Swap::handle_swap(ctx, args, current_slot, current_epoch, current_unix_timestamp, mode)
    }

    /// Permissionlessly closes an hLP after passive funding has exhausted its
    /// marked collateral. Insurance reimburses the borrowed-asset shortfall
    /// first; only the caller-capped remainder is socialized as unpaid funding
    /// interest. Ordinary swaps retain their existing account list.
    pub fn liquidate_exhausted_hlp<'info>(
        ctx: Context<'_, '_, '_, 'info, LiquidateExhaustedHlp<'info>>,
        args: LiquidateExhaustedHlpArgs,
    ) -> Result<()> {
        let current_slot = ctx.accounts.update_and_validate(&args)?;
        LiquidateExhaustedHlp::handle(ctx, args, current_slot)
    }

    // Lending instructions
    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn deposit_collateral<'info>(
        ctx: Context<'_, '_, '_, 'info, DepositCollateral<'info>>,
        args: DepositCollateralArgs,
    ) -> Result<()> {
        DepositCollateral::handle_deposit(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn withdraw_collateral<'info>(
        ctx: Context<'_, '_, '_, 'info, WithdrawCollateral<'info>>,
        args: WithdrawCollateralArgs,
    ) -> Result<()> {
        WithdrawCollateral::handle_withdraw(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn borrow<'info>(ctx: Context<'_, '_, '_, 'info, Borrow<'info>>, args: BorrowArgs) -> Result<()> {
        Borrow::handle_borrow(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn repay<'info>(ctx: Context<'_, '_, '_, 'info, Repay<'info>>, args: RepayArgs) -> Result<()> {
        Repay::handle_repay(ctx, args)
    }

    // Leverage instructions
    pub fn open_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, OpenLeverage<'info>>,
        args: OpenLeverageArgs,
    ) -> Result<()> {
        let clock = Clock::get()?;
        ctx.accounts.validate_at(&args, clock.unix_timestamp)?;
        OpenLeverage::handle_open(ctx, args, clock.slot, clock.epoch, clock.unix_timestamp)
    }

    pub fn close_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, CloseLeverage<'info>>,
        args: CloseLeverageArgs,
    ) -> Result<()> {
        let clock = Clock::get()?;
        ctx.accounts.validate_at(&args, clock.unix_timestamp)?;
        CloseLeverage::handle_close(ctx, args, clock.slot, clock.epoch, clock.unix_timestamp)
    }

    pub fn delegated_close_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, CloseLeverage<'info>>,
        args: DelegatedCloseLeverageArgs,
    ) -> Result<()> {
        let clock = Clock::get()?;
        ctx.accounts.validate_delegated_at(&args, clock.unix_timestamp)?;
        CloseLeverage::handle_delegated_close(ctx, args, clock.slot, clock.epoch, clock.unix_timestamp)
    }

    pub fn increase_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, IncreaseLeverage<'info>>,
        args: IncreaseLeverageArgs,
    ) -> Result<()> {
        let clock = Clock::get()?;
        ctx.accounts.validate_at(&args, clock.unix_timestamp)?;
        IncreaseLeverage::handle_increase(ctx, args, clock.slot, clock.epoch, clock.unix_timestamp)
    }

    pub fn decrease_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, DecreaseLeverage<'info>>,
        args: DecreaseLeverageArgs,
    ) -> Result<()> {
        let clock = Clock::get()?;
        ctx.accounts.validate_at(&args, clock.unix_timestamp)?;
        DecreaseLeverage::handle_decrease(ctx, args, clock.slot, clock.unix_timestamp)
    }

    pub fn add_leverage_margin<'info>(
        ctx: Context<'_, '_, '_, 'info, AddLeverageMargin<'info>>,
        args: AddLeverageMarginArgs,
    ) -> Result<()> {
        let clock = Clock::get()?;
        ctx.accounts.validate_at(&args, clock.unix_timestamp)?;
        AddLeverageMargin::handle_add_margin(ctx, args, clock.slot, clock.epoch)
    }

    pub fn remove_leverage_margin<'info>(
        ctx: Context<'_, '_, '_, 'info, RemoveLeverageMargin<'info>>,
        args: RemoveLeverageMarginArgs,
    ) -> Result<()> {
        let clock = Clock::get()?;
        ctx.accounts.validate_at(&args, clock.unix_timestamp)?;
        RemoveLeverageMargin::handle_remove_margin(ctx, args, clock.slot, clock.unix_timestamp)
    }

    pub fn liquidate_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, LiquidateLeverage<'info>>,
        args: LiquidateLeverageArgs,
    ) -> Result<()> {
        let clock = Clock::get()?;
        ctx.accounts.validate_at(&args, clock.unix_timestamp)?;
        LiquidateLeverage::handle_liquidate(ctx, args, clock.slot, clock.unix_timestamp)
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

    // Liquidation auction trigger
    #[access_control(ctx.accounts.update_and_validate())]
    pub fn trigger_liquidation_auction(ctx: Context<TriggerLiquidationAuction>) -> Result<()> {
        TriggerLiquidationAuction::handle_trigger(ctx)
    }

    // Preview instructions
    pub fn preview_market(ctx: Context<PreviewMarket>) -> Result<MarketPreview> {
        PreviewMarket::handle_preview(ctx)
    }

    pub fn preview_add_liquidity(
        ctx: Context<PreviewAddLiquidity>,
        args: PreviewAddLiquidityArgs,
    ) -> Result<AddLiquidityPreview> {
        PreviewAddLiquidity::handle_preview(ctx, args)
    }

    pub fn preview_swap(ctx: Context<PreviewSwap>, args: PreviewSwapArgs) -> Result<SwapPreview> {
        PreviewSwap::handle_preview(ctx, args)
    }

    pub fn preview_borrow_capacity(
        ctx: Context<PreviewBorrowCapacity>,
        args: PreviewBorrowCapacityArgs,
    ) -> Result<BorrowCapacityPreview> {
        PreviewBorrowCapacity::handle_preview(ctx, args)
    }

    pub fn preview_borrow_position(ctx: Context<PreviewBorrowPosition>) -> Result<BorrowPositionPreview> {
        PreviewBorrowPosition::handle_preview(ctx)
    }

    // Liquidation auction bidding and settlement
    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn bid_liquidation_auction<'info>(
        ctx: Context<'_, '_, '_, 'info, BidLiquidationAuction<'info>>,
        args: BidLiquidationAuctionArgs,
    ) -> Result<()> {
        BidLiquidationAuction::handle_bid(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn settle_liquidation_auction_floor<'info>(
        ctx: Context<'_, '_, '_, 'info, SettleLiquidationAuctionFloor<'info>>,
        args: SettleLiquidationAuctionFloorArgs,
    ) -> Result<()> {
        SettleLiquidationAuctionFloor::handle_settle(ctx, args)
    }

    // HLP instructions
    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn deposit_single_sided<'info>(
        ctx: Context<'_, '_, '_, 'info, DepositSingleSided<'info>>,
        args: DepositSingleSidedArgs,
    ) -> Result<()> {
        DepositSingleSided::handle_deposit(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn withdraw_single_sided<'info>(
        ctx: Context<'_, '_, '_, 'info, WithdrawSingleSided<'info>>,
        args: WithdrawSingleSidedArgs,
    ) -> Result<()> {
        WithdrawSingleSided::handle_withdraw(ctx, args)
    }

    pub fn fallback<'info>(program_id: &Pubkey, accounts: &'info [AccountInfo<'info>], data: &[u8]) -> Result<()> {
        crate::instructions::transfer_hook::handle_transfer_hook(program_id, accounts, data)
    }
}
