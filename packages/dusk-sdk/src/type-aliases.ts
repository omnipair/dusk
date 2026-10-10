import type { IdlTypes } from "@coral-xyz/anchor";
import type { Dusk } from "./types_v2.js";

// Restrict Anchor's recursive type decoding to the definitions reachable from
// account and event roots. The full IDL includes instruction-only types and
// exceeds TypeScript's instantiation depth after the protocol layout grows.
type TypeNamed<N extends Dusk["types"][number]["name"]> = Extract<
  Dusk["types"][number],
  { name: N }
>;
type DuskAccountTypes = [
  TypeNamed<"liquidationConfig">,
  TypeNamed<"liquidationDistress">,
  TypeNamed<"liquidationSession">,
  TypeNamed<"liquidationHealth">,
  TypeNamed<"liquidationRates">,
  TypeNamed<"liquidationFeeAllocation">,
  TypeNamed<"flashLiquidationQuote">,
  TypeNamed<"ammConfig">,
  TypeNamed<"ammState">,
  TypeNamed<"borrowPosition">,
  TypeNamed<"concentratedCurveCache">,
  TypeNamed<"dailyBorrowBucket">,
  TypeNamed<"debt">,
  TypeNamed<"deferredControllerTarget">,
  TypeNamed<"feeProfile">,
  TypeNamed<"fees">,
  TypeNamed<"futarchyAuthority">,
  TypeNamed<"hlpVault">,
  TypeNamed<"insurance">,
  TypeNamed<"insuranceDrawWindow">,
  TypeNamed<"irmConfig">,
  TypeNamed<"leverageDelegation">,
  TypeNamed<"leverageMarginTerms">,
  TypeNamed<"leveragePosition">,
  TypeNamed<"market">,
  TypeNamed<"marketConfig">,
  TypeNamed<"marketParameterUpdate">,
  TypeNamed<"marketSide">,
  TypeNamed<"parameterProposal">,
  TypeNamed<"parameterProposalStatus">,
  TypeNamed<"proposalMetadataV1">,
  TypeNamed<"proposalSupport">,
  TypeNamed<"protocolAuctionConfig">,
  TypeNamed<"protocolAuctionEpoch">,
  TypeNamed<"protocolAuctionParams">,
  TypeNamed<"protocolAuctionRecipients">,
  TypeNamed<"protocolAuctionSplit">,
  TypeNamed<"referralAccrual">,
  TypeNamed<"referralPartner">,
  TypeNamed<"reserveShares">,
  TypeNamed<"reserves">,
  TypeNamed<"revenueDistribution">,
  TypeNamed<"revenueRecipients">,
  TypeNamed<"revenueShare">,
  TypeNamed<"risk">,
  TypeNamed<"virtualYieldLedger">,
  TypeNamed<"yieldAccount">,
];
type DuskEventTypes = [
  TypeNamed<"liquidationConfig">,
  TypeNamed<"liquidationDistress">,
  TypeNamed<"liquidationHealth">,
  TypeNamed<"liquidationRates">,
  TypeNamed<"liquidationFeeAllocation">,
  TypeNamed<"liquidationLossAllocation">,
  TypeNamed<"flashLiquidationQuote">,
  TypeNamed<"emergencyLiquidationQuote">,
  TypeNamed<"flashLiquidationBegun">,
  TypeNamed<"flashLiquidationSettled">,
  TypeNamed<"emergencyLiquidationSettled">,
  TypeNamed<"liquidationObserved">,
  TypeNamed<"ammConfig">,
  TypeNamed<"borrowInterestAccrued">,
  TypeNamed<"borrowInterestPaid">,
  TypeNamed<"borrowPositionLiquidated">,
  TypeNamed<"debtFreePositionClosed">,
  TypeNamed<"debtSource">,
  TypeNamed<"feeProfile">,
  TypeNamed<"harvestAuthorityUpdated">,
  TypeNamed<"hlpClosed">,
  TypeNamed<"hlpOpened">,
  TypeNamed<"hlpTerminalLiquidated">,
  TypeNamed<"insuranceDonated">,
  TypeNamed<"irmConfig">,
  TypeNamed<"leverageDelegationUpdated">,
  TypeNamed<"leverageMarginTerms">,
  TypeNamed<"leveragePositionClosed">,
  TypeNamed<"leveragePositionLiquidated">,
  TypeNamed<"leveragePositionOpened">,
  TypeNamed<"leveragePositionUpdated">,
  TypeNamed<"leverageSwapReceipt">,
  TypeNamed<"liquidationAuctionCancelled">,
  TypeNamed<"liquidationAuctionStarted">,
  TypeNamed<"liquidityAdded">,
  TypeNamed<"liquidityRemoved">,
  TypeNamed<"lpTransferred">,
  TypeNamed<"marketCollateralDeposited">,
  TypeNamed<"marketCollateralWithdrawn">,
  TypeNamed<"marketConfig">,
  TypeNamed<"marketCreated">,
  TypeNamed<"marketDebtUpdated">,
  TypeNamed<"marketEventMetadata">,
  TypeNamed<"marketHealthUpdated">,
  TypeNamed<"marketParameterUpdate">,
  TypeNamed<"marketReduceOnlyUpdated">,
  TypeNamed<"marketSideSnapshot">,
  TypeNamed<"parameterProposalCreated">,
  TypeNamed<"parameterProposalExecuted">,
  TypeNamed<"parameterProposalQueued">,
  TypeNamed<"parameterProposalSupportWithdrawn">,
  TypeNamed<"parameterProposalSupported">,
  TypeNamed<"proposalMetadataV1">,
  TypeNamed<"protocolAuctionConfigUpdated">,
  TypeNamed<"protocolAuctionRecipientsUpdated">,
  TypeNamed<"protocolAuctionRouteUpdated">,
  TypeNamed<"protocolAuctionSettled">,
  TypeNamed<"protocolAuctionSplitUpdated">,
  TypeNamed<"referralBound">,
  TypeNamed<"referralInterestAccrued">,
  TypeNamed<"referralInterestClaimed">,
  TypeNamed<"referralInterestShareCapUpdated">,
  TypeNamed<"referralPartnerConfigured">,
  TypeNamed<"referralRecipientUpdated">,
  TypeNamed<"swapExecuted">,
  TypeNamed<"swapOrigin">,
  TypeNamed<"yieldClaimed">,
  TypeNamed<"yieldRecipientUpdated">,
];
type DuskAccountIdl = {
  address: Dusk["address"];
  metadata: Dusk["metadata"];
  instructions: [];
  accounts: Dusk["accounts"];
  events: [];
  types: DuskAccountTypes;
};
type DuskEventIdl = {
  address: Dusk["address"];
  metadata: Dusk["metadata"];
  instructions: [];
  accounts: [];
  events: Dusk["events"];
  types: DuskEventTypes;
};

export type Market = IdlTypes<DuskAccountIdl>["market"];
export type BorrowPosition = IdlTypes<DuskAccountIdl>["borrowPosition"];
export type LeveragePosition = IdlTypes<DuskAccountIdl>["leveragePosition"];
export type LeverageDelegation = IdlTypes<DuskAccountIdl>["leverageDelegation"];
export type YieldAccount = IdlTypes<DuskAccountIdl>["yieldAccount"];
export type FutarchyAuthority = IdlTypes<DuskAccountIdl>["futarchyAuthority"];
export type ReferralPartner = IdlTypes<DuskAccountIdl>["referralPartner"];
export type ReferralAccrual = IdlTypes<DuskAccountIdl>["referralAccrual"];
export type ParameterProposal = IdlTypes<DuskAccountIdl>["parameterProposal"];
export type ProposalSupport = IdlTypes<DuskAccountIdl>["proposalSupport"];

export type HlpClosed = IdlTypes<DuskEventIdl>["hlpClosed"];
export type HlpOpened = IdlTypes<DuskEventIdl>["hlpOpened"];
export type LiquidationAuctionCancelled = IdlTypes<DuskEventIdl>["liquidationAuctionCancelled"];
export type LiquidationAuctionStarted = IdlTypes<DuskEventIdl>["liquidationAuctionStarted"];
export type LiquidityAdded = IdlTypes<DuskEventIdl>["liquidityAdded"];
export type LiquidityRemoved = IdlTypes<DuskEventIdl>["liquidityRemoved"];
export type LpTransferred = IdlTypes<DuskEventIdl>["lpTransferred"];
export type MarketCollateralDeposited = IdlTypes<DuskEventIdl>["marketCollateralDeposited"];
export type MarketCollateralWithdrawn = IdlTypes<DuskEventIdl>["marketCollateralWithdrawn"];
export type MarketCreated = IdlTypes<DuskEventIdl>["marketCreated"];
export type MarketDebtUpdated = IdlTypes<DuskEventIdl>["marketDebtUpdated"];
export type MarketHealthUpdated = IdlTypes<DuskEventIdl>["marketHealthUpdated"];
export type MarketReduceOnlyUpdated = IdlTypes<DuskEventIdl>["marketReduceOnlyUpdated"];
export type ParameterProposalCreated = IdlTypes<DuskEventIdl>["parameterProposalCreated"];
export type ParameterProposalExecuted = IdlTypes<DuskEventIdl>["parameterProposalExecuted"];
export type ParameterProposalQueued = IdlTypes<DuskEventIdl>["parameterProposalQueued"];
export type ParameterProposalSupported = IdlTypes<DuskEventIdl>["parameterProposalSupported"];
export type ParameterProposalSupportWithdrawn =
  IdlTypes<DuskEventIdl>["parameterProposalSupportWithdrawn"];
export type BorrowPositionLiquidated = IdlTypes<DuskEventIdl>["borrowPositionLiquidated"];
export type BorrowInterestAccrued = IdlTypes<DuskEventIdl>["borrowInterestAccrued"];
export type BorrowInterestPaid = IdlTypes<DuskEventIdl>["borrowInterestPaid"];
export type DebtSource = BorrowInterestPaid["source"];
export type LeveragePositionOpened = IdlTypes<DuskEventIdl>["leveragePositionOpened"];
export type LeveragePositionUpdated = IdlTypes<DuskEventIdl>["leveragePositionUpdated"];
export type LeveragePositionClosed = IdlTypes<DuskEventIdl>["leveragePositionClosed"];
export type LeveragePositionLiquidated = IdlTypes<DuskEventIdl>["leveragePositionLiquidated"];
export type ProtocolAuctionSettled = IdlTypes<DuskEventIdl>["protocolAuctionSettled"];
export type ReferralBound = IdlTypes<DuskEventIdl>["referralBound"];
export type ReferralPartnerConfigured = IdlTypes<DuskEventIdl>["referralPartnerConfigured"];
export type ReferralInterestAccrued = IdlTypes<DuskEventIdl>["referralInterestAccrued"];
export type ReferralInterestClaimed = IdlTypes<DuskEventIdl>["referralInterestClaimed"];
export type ReferralInterestShareCapUpdated = IdlTypes<DuskEventIdl>["referralInterestShareCapUpdated"];
export type ReferralRecipientUpdated = IdlTypes<DuskEventIdl>["referralRecipientUpdated"];
export type SwapExecuted = IdlTypes<DuskEventIdl>["swapExecuted"];
export type SwapOrigin = SwapExecuted["origin"];
export type MarketSideSnapshot = SwapExecuted["base"];
export type YieldClaimed = IdlTypes<DuskEventIdl>["yieldClaimed"];
export type YieldRecipientUpdated = IdlTypes<DuskEventIdl>["yieldRecipientUpdated"];

export type LiquidationSession = IdlTypes<DuskAccountIdl>["liquidationSession"];
export type FlashLiquidationBegun = IdlTypes<DuskEventIdl>["flashLiquidationBegun"];
export type FlashLiquidationSettled = IdlTypes<DuskEventIdl>["flashLiquidationSettled"];
export type EmergencyLiquidationSettled = IdlTypes<DuskEventIdl>["emergencyLiquidationSettled"];
export type LiquidationObserved = IdlTypes<DuskEventIdl>["liquidationObserved"];
