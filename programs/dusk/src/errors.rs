use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    #[msg("Invalid deployer")]
    InvalidDeployer,

    #[msg("Argument missing")]
    ArgumentMissing,

    #[msg("Invalid swap fee bps")]
    InvalidSwapFeeBps,

    #[msg("Invalid interest fee bps")]
    InvalidInterestFeeBps,

    #[msg("Invalid half life")]
    InvalidHalfLife,

    #[msg("Invalid futarchy authority")]
    InvalidFutarchyAuthority,

    #[msg("Invalid reduce-only authority")]
    InvalidReduceOnlyAuthority,

    #[msg("Invalid parameter proposal metadata")]
    InvalidProposalMetadata,

    #[msg("Invalid parameter proposal description URI")]
    InvalidProposalUri,

    #[msg("Parameter proposal digest does not match its immutable contents")]
    InvalidProposalDigest,

    #[msg("Invalid parameter proposal account")]
    InvalidParameterProposal,

    #[msg("Invalid parameter proposal support account")]
    InvalidProposalSupport,

    #[msg("Parameter proposal is not collecting support")]
    ProposalNotCollecting,

    #[msg("Parameter proposal is not queued")]
    ProposalNotQueued,

    #[msg("Queued proposal support is frozen")]
    ProposalSupportFrozen,

    #[msg("Proposal support is below the sponsorship floor")]
    ProposalSponsorshipTooLow,

    #[msg("Proposal does not have a strict majority of eligible yLP")]
    ProposalSupportInsufficient,

    #[msg("Parameter proposal timelock is not ready")]
    ProposalTimelockNotReady,

    #[msg("Parameter proposal execution window has expired")]
    ProposalExecutionWindowExpired,

    #[msg("Parameter proposal was invalidated by a same-family update")]
    ProposalStale,

    #[msg("Parameter update would not change the active market parameters")]
    ParameterUpdateNotMeaningful,

    #[msg("Invalid parameter update")]
    InvalidParameterUpdate,

    #[msg("Parameter execution is blocked while either lending side is at or above 80% utilization")]
    UtilizationGuardExceeded,

    #[msg("Invalid argument")]
    InvalidArgument,

    #[msg("Amount cannot be zero")]
    AmountZero,

    #[msg("Insufficient amount0 in")]
    InsufficientAmount0In,

    #[msg("Insufficient amount1 in")]
    InsufficientAmount1In,

    #[msg("Borrowing power exceeded")]
    BorrowingPowerExceeded,

    #[msg("Invalid token account")]
    InvalidTokenAccount,

    #[msg("Invalid token program")]
    InvalidTokenProgram,

    #[msg("Borrow exceeds reserve")]
    BorrowExceedsReserve,

    #[msg("Insufficient amount0")]
    InsufficientAmount0,

    #[msg("Insufficient amount1")]
    InsufficientAmount1,

    #[msg("Insufficient output amount")]
    InsufficientOutputAmount,

    #[msg("Output amount below minimum requested (slippage exceeded)")]
    SlippageExceeded,

    #[msg("Insufficient liquidity")]
    InsufficientLiquidity,

    #[msg("Insufficient cash reserve0")]
    InsufficientCashReserve0,

    #[msg("Insufficient cash reserve1")]
    InsufficientCashReserve1,

    #[msg("Arithmetic overflow")]
    Overflow,

    #[msg("Undercollateralized")]
    Undercollateralized,

    #[msg("Insufficient balance for collateral")]
    InsufficientBalanceForCollateral,

    #[msg("Insufficient amount")]
    InsufficientAmount,

    #[msg("User balance insufficient to cover requested amount")]
    InsufficientBalance,

    #[msg("Insufficient debt")]
    InsufficientDebt,

    #[msg("User position not initialized")]
    UserPositionNotInitialized,

    #[msg("Zero debt amount")]
    ZeroDebtAmount,

    #[msg("Not undercollateralized")]
    NotUndercollateralized,

    #[msg("Broken invariant")]
    BrokenInvariant,

    #[msg("Math overflow during invariant calculation")]
    InvariantOverflow,

    #[msg("Math overflow during fee calculation.")]
    FeeMathOverflow,

    #[msg("Math overflow during output amount calculation.")]
    OutputAmountOverflow,

    #[msg("Math overflow during reserve calculation.")]
    ReserveOverflow,

    #[msg("Math underflow during reserve calculation.")]
    ReserveUnderflow,

    #[msg("Math underflow during cash reserve calculation.")]
    CashReserveUnderflow,

    #[msg("Math overflow during denominator calculation.")]
    DenominatorOverflow,

    #[msg("Math overflow during liquidity calculation")]
    LiquidityMathOverflow,

    #[msg("Math overflow during liquidity square root calculation")]
    LiquiditySqrtOverflow,

    #[msg("Math underflow during liquidity calculation")]
    LiquidityUnderflow,

    #[msg("Math overflow during liquidity conversion")]
    LiquidityConversionOverflow,

    #[msg("Math overflow during supply calculation")]
    SupplyOverflow,

    #[msg("Math underflow during supply calculation")]
    SupplyUnderflow,

    #[msg("Math overflow during debt calculation")]
    DebtMathOverflow,

    #[msg("Math overflow during debt share calculation")]
    DebtShareMathOverflow,

    #[msg("Math overflow during debt share division")]
    DebtShareDivisionOverflow,

    #[msg("Math overflow during debt utilization calculation")]
    DebtUtilizationOverflow,

    #[msg("Invalid mint")]
    InvalidMint,

    #[msg("Invalid mint length")]
    InvalidMintLen,

    #[msg("Invalid distribution - percentages must sum to 100%")]
    InvalidDistribution,

    #[msg("Invalid protocol auction config")]
    InvalidAuctionConfig,

    #[msg("Protocol auction reference price is stale")]
    StaleAuctionReference,

    #[msg("Protocol auction payment is insufficient")]
    InsufficientAuctionPayment,

    #[msg("Invalid LP mint key")]
    InvalidLpMintKey,

    #[msg("Invalid LP name")]
    InvalidLpName,

    #[msg("Invalid LP symbol")]
    InvalidLpSymbol,

    #[msg("Invalid LP URI")]
    InvalidLpUri,

    #[msg("Account not empty")]
    AccountNotEmpty,

    #[msg("Invalid mint authority")]
    InvalidMintAuthority,

    #[msg("Frozen LP mint")]
    FrozenLpMint,

    #[msg("Non-zero supply")]
    NonZeroSupply,

    #[msg("Wrong LP decimals")]
    WrongLpDecimals,

    #[msg("Asset mint decimals exceed Dusk's 9-decimal AMM precision")]
    UnsupportedAssetDecimals,

    #[msg("Invalid vault - token_in_vault and token_out_vault must be different")]
    InvalidVaultSameAccount,

    #[msg("Invalid vault")]
    InvalidVault,

    #[msg("Invalid params hash - hash does not match computed parameters")]
    InvalidParamsHash,

    #[msg("Invalid version")]
    InvalidVersion,

    #[msg("Invalid token order")]
    InvalidTokenOrder,

    #[msg("Invalid rate model - rate_model does not match market configuration")]
    InvalidRateModel,

    #[msg("Invalid position market - position does not match market")]
    InvalidPositionMarket,

    #[msg("Invalid utilization bounds - must satisfy: MIN <= start < end <= MAX")]
    InvalidUtilBounds,

    #[msg("Invalid rate parameters - check half_life_ms, min_rate_bps, max_rate_bps, initial_rate_bps bounds")]
    InvalidRateParams,

    #[msg("Operation blocked: reduce-only mode is active")]
    ReduceOnlyMode,

    #[msg("Cannot remove collateral in reduce-only mode while debt exists")]
    ReduceOnlyHasDebt,

    #[msg("Invalid instructions sysvar")]
    InvalidInstructionsSysvar,

    #[msg("Insufficient post-withdraw debt coverage")]
    InsufficientPostWithdrawDebtCoverage,

    #[msg("Invalid recipient - address does not match configured revenue recipient")]
    InvalidRecipient,

    #[msg("Invalid market")]
    InvalidMarket,

    #[msg("Invalid market config")]
    InvalidMarketConfig,

    #[msg("Invalid settlement price")]
    InvalidSettlementPrice,

    #[msg("Market reserve share backing is insufficient")]
    InsufficientMarketShareBacking,

    #[msg("Invalid market side")]
    InvalidMarketSide,

    #[msg("Invalid yield account")]
    InvalidYieldAccount,

    #[msg("Invalid hLP vault")]
    InvalidHlpVault,

    #[msg("Live hLP mint supply is inconsistent with stored vault supply")]
    InvalidHlpMintSupply,

    #[msg("Not enough remaining accounts")]
    NotEnoughAccounts,

    #[msg("hLP settlement is unavailable")]
    HlpSettlementUnavailable,

    #[msg("hLP funding position is not eligible for permissionless liquidation")]
    HlpNotLiquidatable,

    #[msg("Borrow headroom is insufficient")]
    InsufficientBorrowHeadroom,

    #[msg("Market health is insufficient")]
    InsufficientMarketHealth,

    #[msg("Invalid borrow position")]
    InvalidBorrowPosition,

    #[msg("Position is not liquidatable")]
    PositionNotLiquidatable,

    #[msg("Insurance coverage is insufficient")]
    InsufficientInsurance,

    #[msg("Socialized liquidation loss exceeds caller cap")]
    LiquidationSocializationExceeded,

    #[msg("Claim mint must not charge transfer fees")]
    InvalidClaimMint,

    #[msg("Fee liability is not backed by its custody balance")]
    UnbackedFeeLiability,

    #[msg("Invalid market fee authority")]
    InvalidMarketFeeAuthority,

    #[msg("Market is reduce-only")]
    MarketReduceOnly,

    #[msg("Market has not started")]
    MarketNotStarted,

    #[msg("Market math overflow")]
    MarketMathOverflow,

    #[msg("Daily liquidity limit exceeded")]
    DailyLimitExceeded,

    #[msg("Instruction is intentionally not live yet")]
    InstructionNotLive,

    #[msg("Liquidation repay amount exceeds partial liquidation cap")]
    LiquidationRepayTooLarge,

    #[msg("Leverage multiplier exceeds circuit breaker")]
    LeverageMultiplierTooHigh,

    #[msg("Leverage position does not have enough initial margin")]
    LeverageInitialMarginTooLow,

    #[msg("Leverage unwind impact exceeds limit")]
    LeverageUnwindImpactTooHigh,

    #[msg("Leverage position is not liquidatable")]
    LeveragePositionNotLiquidatable,

    #[msg("Invalid signer")]
    InvalidSigner,

    #[msg("Invalid leverage position")]
    InvalidLeveragePosition,

    #[msg("Invalid leverage delegation")]
    InvalidLeverageDelegation,

    #[msg("Referral interest share exceeds the protocol hard cap")]
    InvalidReferralInterestShareBps,

    #[msg("Invalid referral partner")]
    InvalidReferralPartner,

    #[msg("Referral partner is not active")]
    ReferralPartnerNotActive,

    #[msg("Invalid referral accrual account")]
    InvalidReferralAccrual,

    #[msg("Leverage collateral mint must not have transfer fee configuration")]
    InvalidLeverageCollateralMint,
}
