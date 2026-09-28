import type { IdlAccounts, IdlEvents } from "@coral-xyz/anchor";
import type { Dusk } from "./types_v2.js";

// Restrict Anchor's recursive type decoding to the definitions reachable from
// account and event roots. The full IDL includes instruction-only types and
// exceeds TypeScript's instantiation depth after the protocol layout grows.
type TypeNamed<N extends Dusk["types"][number]["name"]> = Extract<
  Dusk["types"][number],
  { name: N }
>;
type DuskAccountTypes = [
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
  TypeNamed<"leveragePosition">,
  TypeNamed<"market">,
  TypeNamed<"marketConfig">,
  TypeNamed<"marketParameterUpdate">,
  TypeNamed<"marketSide">,
  TypeNamed<"parameterFamily">,
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
  TypeNamed<"ammConfig">,
  TypeNamed<"borrowInterestAccrued">,
  TypeNamed<"borrowInterestPaid">,
  TypeNamed<"borrowPositionLiquidated">,
  TypeNamed<"debtFreePositionClosed">,
  TypeNamed<"debtSource">,
  TypeNamed<"harvestAuthorityUpdated">,
  TypeNamed<"hlpClosed">,
  TypeNamed<"hlpOpened">,
  TypeNamed<"hlpTerminalLiquidated">,
  TypeNamed<"insuranceDonated">,
  TypeNamed<"irmConfig">,
  TypeNamed<"leverageDelegationUpdated">,
  TypeNamed<"leveragePositionClosed">,
  TypeNamed<"leveragePositionLiquidated">,
  TypeNamed<"leveragePositionOpened">,
  TypeNamed<"leveragePositionUpdated">,
  TypeNamed<"leverageSwapReceipt">,
  TypeNamed<"liquidityAdded">,
  TypeNamed<"liquidityRemoved">,
  TypeNamed<"marketCollateralDeposited">,
  TypeNamed<"marketCollateralWithdrawn">,
  TypeNamed<"marketConfig">,
  TypeNamed<"marketCreated">,
  TypeNamed<"marketDebtUpdated">,
  TypeNamed<"marketEventMetadata">,
  TypeNamed<"marketHealthUpdated">,
  TypeNamed<"marketReduceOnlyUpdated">,
  TypeNamed<"parameterProposalCreated">,
  TypeNamed<"parameterProposalExecuted">,
  TypeNamed<"parameterProposalQueued">,
  TypeNamed<"parameterProposalSupportWithdrawn">,
  TypeNamed<"parameterProposalSupported">,
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

export type Market = IdlAccounts<DuskAccountIdl>["market"];
export type BorrowPosition = IdlAccounts<DuskAccountIdl>["borrowPosition"];
export type LeveragePosition = IdlAccounts<DuskAccountIdl>["leveragePosition"];
export type LeverageDelegation = IdlAccounts<DuskAccountIdl>["leverageDelegation"];
export type YieldAccount = IdlAccounts<DuskAccountIdl>["yieldAccount"];
export type FutarchyAuthority = IdlAccounts<DuskAccountIdl>["futarchyAuthority"];
export type ReferralPartner = IdlAccounts<DuskAccountIdl>["referralPartner"];
export type ReferralAccrual = IdlAccounts<DuskAccountIdl>["referralAccrual"];
export type ParameterProposal = IdlAccounts<DuskAccountIdl>["parameterProposal"];
export type ProposalSupport = IdlAccounts<DuskAccountIdl>["proposalSupport"];

export type HlpClosed = IdlEvents<DuskEventIdl>["hlpClosed"];
export type HlpOpened = IdlEvents<DuskEventIdl>["hlpOpened"];
export type LiquidityAdded = IdlEvents<DuskEventIdl>["liquidityAdded"];
export type LiquidityRemoved = IdlEvents<DuskEventIdl>["liquidityRemoved"];
export type MarketCollateralDeposited = IdlEvents<DuskEventIdl>["marketCollateralDeposited"];
export type MarketCollateralWithdrawn = IdlEvents<DuskEventIdl>["marketCollateralWithdrawn"];
export type MarketCreated = IdlEvents<DuskEventIdl>["marketCreated"];
export type MarketDebtUpdated = IdlEvents<DuskEventIdl>["marketDebtUpdated"];
export type MarketHealthUpdated = IdlEvents<DuskEventIdl>["marketHealthUpdated"];
export type MarketReduceOnlyUpdated = IdlEvents<DuskEventIdl>["marketReduceOnlyUpdated"];
export type ParameterProposalCreated = IdlEvents<DuskEventIdl>["parameterProposalCreated"];
export type ParameterProposalExecuted = IdlEvents<DuskEventIdl>["parameterProposalExecuted"];
export type ParameterProposalQueued = IdlEvents<DuskEventIdl>["parameterProposalQueued"];
export type ParameterProposalSupported = IdlEvents<DuskEventIdl>["parameterProposalSupported"];
export type ParameterProposalSupportWithdrawn =
  IdlEvents<DuskEventIdl>["parameterProposalSupportWithdrawn"];
export type BorrowPositionLiquidated = IdlEvents<DuskEventIdl>["borrowPositionLiquidated"];
export type BorrowInterestAccrued = IdlEvents<DuskEventIdl>["borrowInterestAccrued"];
export type BorrowInterestPaid = IdlEvents<DuskEventIdl>["borrowInterestPaid"];
export type DebtSource = BorrowInterestPaid["source"];
export type LeveragePositionOpened = IdlEvents<DuskEventIdl>["leveragePositionOpened"];
export type LeveragePositionUpdated = IdlEvents<DuskEventIdl>["leveragePositionUpdated"];
export type LeveragePositionClosed = IdlEvents<DuskEventIdl>["leveragePositionClosed"];
export type LeveragePositionLiquidated = IdlEvents<DuskEventIdl>["leveragePositionLiquidated"];
export type ProtocolAuctionSettled = IdlEvents<DuskEventIdl>["protocolAuctionSettled"];
export type ReferralBound = IdlEvents<DuskEventIdl>["referralBound"];
export type ReferralPartnerConfigured = IdlEvents<DuskEventIdl>["referralPartnerConfigured"];
export type ReferralInterestAccrued = IdlEvents<DuskEventIdl>["referralInterestAccrued"];
export type ReferralInterestClaimed = IdlEvents<DuskEventIdl>["referralInterestClaimed"];
export type ReferralInterestShareCapUpdated = IdlEvents<DuskEventIdl>["referralInterestShareCapUpdated"];
export type ReferralRecipientUpdated = IdlEvents<DuskEventIdl>["referralRecipientUpdated"];
export type SwapExecuted = IdlEvents<DuskEventIdl>["swapExecuted"];
export type SwapOrigin = SwapExecuted["origin"];
export type YieldClaimed = IdlEvents<DuskEventIdl>["yieldClaimed"];
export type YieldRecipientUpdated = IdlEvents<DuskEventIdl>["yieldRecipientUpdated"];
