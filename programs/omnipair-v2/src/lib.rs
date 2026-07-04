use anchor_lang::prelude::*;

pub mod constants;
pub mod errors;
pub mod events;
pub mod instructions;
pub mod math;
pub mod shared;
pub mod state;

pub use instructions::*;
pub use state::*;

#[cfg(not(feature = "no-entrypoint"))]
use solana_security_txt::security_txt;

#[cfg(not(feature = "no-entrypoint"))]
security_txt! {
    name: "Omnipair V2",
    project_url: "https://omnipair.fi",
    contacts: "email:security@omnipair.fi,telegram:rustfully",
    source_code: "https://github.com/omnipair/dusk",
    source_release: env!("GIT_RELEASE"),
    source_revision: env!("GIT_REV"),
    auditors: "Pending final V2 security review",
    policy: "https://omnipair.fi/security"
}

declare_id!("358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv");

#[program]
pub mod omnipair_v2 {
    use super::*;

    pub fn init_futarchy_authority(
        ctx: Context<InitFutarchyAuthority>,
        args: InitFutarchyAuthorityArgs,
    ) -> Result<()> {
        InitFutarchyAuthority::handle_init(ctx, args)
    }

    pub fn update_futarchy_authority(
        ctx: Context<UpdateFutarchyAuthority>,
        args: UpdateFutarchyAuthorityArgs,
    ) -> Result<()> {
        UpdateFutarchyAuthority::handle_update(ctx, args)
    }

    pub fn update_protocol_revenue(
        ctx: Context<UpdateProtocolRevenue>,
        args: UpdateProtocolRevenueArgs,
    ) -> Result<()> {
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

    pub fn set_global_reduce_only(
        ctx: Context<SetGlobalReduceOnly>,
        args: SetGlobalReduceOnlyArgs,
    ) -> Result<()> {
        SetGlobalReduceOnly::handle_set_global_reduce_only(ctx, args)
    }

    #[access_control(ctx.accounts.validate(&args))]
    pub fn settle_protocol_auction<'info>(
        ctx: Context<'_, '_, '_, 'info, SettleProtocolAuction<'info>>,
        args: SettleProtocolAuctionArgs,
    ) -> Result<()> {
        SettleProtocolAuction::handle_settle(ctx, args)
    }

    #[access_control(ctx.accounts.validate(&args))]
    pub fn initialize(ctx: Context<InitializeMarket>, args: InitializeMarketArgs) -> Result<()> {
        InitializeMarket::handle_initialize(ctx, args)
    }

    #[access_control(ctx.accounts.validate(&args))]
    pub fn initialize_lp_metadata(
        ctx: Context<InitializeLpMetadata>,
        args: InitializeLpMetadataArgs,
    ) -> Result<()> {
        InitializeLpMetadata::handle_initialize(ctx, args)
    }

    pub fn update_config(
        ctx: Context<UpdateMarketConfig>,
        args: UpdateMarketConfigArgs,
    ) -> Result<()> {
        UpdateMarketConfig::handle_update(ctx, args)
    }

    #[access_control(ctx.accounts.validate())]
    pub fn set_reduce_only(
        ctx: Context<SetMarketReduceOnly>,
        args: SetMarketReduceOnlyArgs,
    ) -> Result<()> {
        SetMarketReduceOnly::handle_set(ctx, args)
    }

    pub fn set_operator(ctx: Context<SetMarketAuthority>, args: SetOperatorArgs) -> Result<()> {
        SetMarketAuthority::handle_set_operator(ctx, args)
    }

    pub fn set_manager(ctx: Context<SetMarketAuthority>, args: SetManagerArgs) -> Result<()> {
        SetMarketAuthority::handle_set_manager(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate())]
    pub fn claim_manager_fees(ctx: Context<ClaimManagerFees>) -> Result<()> {
        ClaimManagerFees::handle_claim(ctx)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn add_liquidity(ctx: Context<AddLiquidity>, args: AddLiquidityArgs) -> Result<()> {
        AddLiquidity::handle_add_liquidity(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn remove_liquidity(
        ctx: Context<RemoveLiquidity>,
        args: RemoveLiquidityArgs,
    ) -> Result<()> {
        RemoveLiquidity::handle_remove_liquidity(ctx, args)
    }

    #[access_control(ctx.accounts.validate(&args))]
    pub fn set_yield_recipient(
        ctx: Context<SetYieldRecipient>,
        args: SetYieldRecipientArgs,
    ) -> Result<()> {
        SetYieldRecipient::handle_set(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn claim_yield(ctx: Context<ClaimYield>, args: ClaimYieldArgs) -> Result<()> {
        ClaimYield::handle_claim(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn swap<'info>(ctx: Context<'_, '_, '_, 'info, Swap<'info>>, args: SwapArgs) -> Result<()> {
        Swap::handle_swap(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn deposit_collateral(
        ctx: Context<DepositCollateral>,
        args: DepositCollateralArgs,
    ) -> Result<()> {
        DepositCollateral::handle_deposit(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn withdraw_collateral(
        ctx: Context<WithdrawCollateral>,
        args: WithdrawCollateralArgs,
    ) -> Result<()> {
        WithdrawCollateral::handle_withdraw(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn borrow(ctx: Context<Borrow>, args: BorrowArgs) -> Result<()> {
        Borrow::handle_borrow(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn repay(ctx: Context<Repay>, args: RepayArgs) -> Result<()> {
        Repay::handle_repay(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn open_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, OpenLeverage<'info>>,
        args: OpenLeverageArgs,
    ) -> Result<()> {
        OpenLeverage::handle_open(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn close_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, CloseLeverage<'info>>,
        args: CloseLeverageArgs,
    ) -> Result<()> {
        CloseLeverage::handle_close(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate_delegated(&args))]
    pub fn delegated_close_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, CloseLeverage<'info>>,
        args: DelegatedCloseLeverageArgs,
    ) -> Result<()> {
        CloseLeverage::handle_delegated_close(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn increase_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, IncreaseLeverage<'info>>,
        args: IncreaseLeverageArgs,
    ) -> Result<()> {
        IncreaseLeverage::handle_increase(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn decrease_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, DecreaseLeverage<'info>>,
        args: DecreaseLeverageArgs,
    ) -> Result<()> {
        DecreaseLeverage::handle_decrease(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn add_leverage_margin<'info>(
        ctx: Context<'_, '_, '_, 'info, AddLeverageMargin<'info>>,
        args: AddLeverageMarginArgs,
    ) -> Result<()> {
        AddLeverageMargin::handle_add_margin(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn remove_leverage_margin<'info>(
        ctx: Context<'_, '_, '_, 'info, RemoveLeverageMargin<'info>>,
        args: RemoveLeverageMarginArgs,
    ) -> Result<()> {
        RemoveLeverageMargin::handle_remove_margin(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn liquidate_leverage<'info>(
        ctx: Context<'_, '_, '_, 'info, LiquidateLeverage<'info>>,
        args: LiquidateLeverageArgs,
    ) -> Result<()> {
        LiquidateLeverage::handle_liquidate(ctx, args)
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

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn liquidate_borrow_position(
        ctx: Context<LiquidateBorrowPosition>,
        args: LiquidateBorrowPositionArgs,
    ) -> Result<()> {
        LiquidateBorrowPosition::handle_liquidate(ctx, args)
    }

    pub fn preview_market(ctx: Context<PreviewMarket>) -> Result<MarketPreview> {
        PreviewMarket::handle_preview(ctx)
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

    pub fn preview_borrow_position(
        ctx: Context<PreviewBorrowPosition>,
    ) -> Result<BorrowPositionPreview> {
        PreviewBorrowPosition::handle_preview(ctx)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn deposit_single_sided(
        ctx: Context<DepositSingleSided>,
        args: DepositSingleSidedArgs,
    ) -> Result<()> {
        DepositSingleSided::handle_deposit(ctx, args)
    }

    #[access_control(ctx.accounts.update_and_validate(&args))]
    pub fn withdraw_single_sided(
        ctx: Context<WithdrawSingleSided>,
        args: WithdrawSingleSidedArgs,
    ) -> Result<()> {
        WithdrawSingleSided::handle_withdraw(ctx, args)
    }

    pub fn fallback<'info>(
        program_id: &Pubkey,
        accounts: &'info [AccountInfo<'info>],
        data: &[u8],
    ) -> Result<()> {
        crate::instructions::transfer_hook::handle_transfer_hook(program_id, accounts, data)
    }
}
