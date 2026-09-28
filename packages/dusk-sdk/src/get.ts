import type { BN, Program } from "@coral-xyz/anchor";
import { PublicKey, type Commitment, type TransactionInstruction } from "@solana/web3.js";
import {
  simulatePreviewWithContext,
  type PreviewSimulationOptions,
  type SimulateOptions,
} from "./simulation.js";
export { DuskSimulationError, DuskPreviewTimeoutError } from "./simulation.js";
export type {
  SimulateOptions,
  PreviewSimulationOptions,
  DuskPreviewSimulation,
} from "./simulation.js";
import {
  previewVirtualBookSnapshot,
  previewVirtualBookQuotes,
  previewVirtualBookBatch,
  projectDuskVirtualBook,
  type DuskVirtualBookSnapshot,
  type VirtualBookQuoteOptions,
  type VirtualBookQuoteRequest,
} from "./virtual-book/index.js";

import {
  deriveBorrowPositionAddress,
  deriveFutarchyAuthorityAddress,
  deriveHlpYlpVaultAddress,
  deriveInsuranceAddress,
  deriveLeveragePositionAddress,
  deriveMarketAddress,
  deriveMarketCollateralVaultAddress,
  deriveMarketInterestVaultAddress,
  deriveMarketReserveVaultAddress,
  deriveParameterProposalAddress,
  deriveProposalSupportAddress,
  deriveReferralAccrualAddress,
  deriveReferralPartnerAddress,
  deriveTokenMetadataAddress,
  deriveYieldAccountAddress,
  deriveYieldTransferHookValidationAddress,
  type U64SeedLike,
} from "./constants.js";
import { address, DEFAULT_READONLY_PUBLIC_KEY, normalizeAccountKeys, type AddressLike } from "./address.js";
import {
  decodePreviewAddLiquidityReturnData,
  decodePreviewBorrowCapacityReturnData,
  decodePreviewBorrowPositionReturnData,
  decodePreviewBorrowPositionCapacityReturnData,
  decodePreviewMarketReturnData,
  decodePreviewSwapReturnData,
  type AddLiquidityPreview,
  type BorrowCapacityPreview,
  type BorrowPositionPreview,
  type BorrowPositionCapacityPreview,
  type MarketPreview,
  type PreviewReturnData,
  type SwapPreview,
} from "./preview.js";
import type {
  BorrowPosition,
  FutarchyAuthority,
  LeverageDelegation,
  LeveragePosition,
  Market,
  ParameterProposal,
  ProposalSupport,
  ReferralAccrual,
  ReferralPartner,
  YieldAccount,
} from "./type-aliases.js";
import type { Dusk } from "./types_v2.js";

export const pda = {
  futarchyAuthority: deriveFutarchyAuthorityAddress,
  market: deriveMarketAddress,
  tokenMetadata: deriveTokenMetadataAddress,
  marketReserveVault: deriveMarketReserveVaultAddress,
  marketCollateralVault: deriveMarketCollateralVaultAddress,
  marketInterestVault: deriveMarketInterestVaultAddress,
  borrowPosition: deriveBorrowPositionAddress,
  leveragePosition: deriveLeveragePositionAddress,
  yieldAccount: deriveYieldAccountAddress,
  yieldTransferHookValidation: deriveYieldTransferHookValidationAddress,
  hlpYlpVault: deriveHlpYlpVaultAddress,
  insurance: deriveInsuranceAddress,
  referralPartner: deriveReferralPartnerAddress,
  referralAccrual: deriveReferralAccrualAddress,
  parameterProposal: deriveParameterProposalAddress,
  proposalSupport: deriveProposalSupportAddress,
} as const;

export interface PreviewSwapParams extends SimulateOptions {
  market: AddressLike;
  futarchyAuthority?: AddressLike;
  assetInMint: AddressLike;
  assetOutMint: AddressLike;
  exactAssetIn: BN;
}

export interface PreviewAddLiquidityParams extends SimulateOptions {
  market: AddressLike;
  baseMint: AddressLike;
  quoteMint: AddressLike;
  baseDepositAmount: BN;
  quoteDepositAmount: BN;
}

export interface PreviewBorrowCapacityParams extends SimulateOptions {
  market: AddressLike;
  collateralAssetMint: AddressLike;
  debtAssetMint: AddressLike;
  collateralAmount: BN;
  /**
   * Candidate debt amount used for the returned CF and health fields. When
   * omitted, the program quotes at maximum borrow capacity.
   */
  projectedBorrowAmount?: BN | null;
}

export interface PreviewBorrowPositionCapacityParams extends SimulateOptions {
  capacityKind: "borrow" | "withdraw";
  market: AddressLike;
  borrowPosition: AddressLike;
  collateralAssetMint: AddressLike;
  debtAssetMint: AddressLike;
  /** Net credit after transfer fees, or negative collateral vault debit. */
  collateralChange: BN;
  /** Additional debt; omit for maximum additional capacity. */
  projectedBorrowAmount?: BN | null;
}

export interface PreviewBorrowPositionParams extends SimulateOptions {
  market: AddressLike;
  borrowPosition: AddressLike;
}

export class DuskGet {
  readonly pda = pda;

  constructor(
    readonly program: Program<Dusk>,
    private readonly defaultFeePayer: AddressLike = DEFAULT_READONLY_PUBLIC_KEY
  ) {}

  async accountInfo(account: AddressLike, commitment?: Commitment) {
    return this.program.provider.connection.getAccountInfo(address(account), commitment);
  }

  async programAccount<T = unknown>(name: string, account: AddressLike): Promise<T> {
    const client = (
      this.program.account as unknown as Record<string, { fetch(address: PublicKey): Promise<T> }>
    )[name];
    if (!client) {
      throw new Error(`Unknown Dusk account type: ${name}`);
    }
    return client.fetch(address(account));
  }

  private allProgramAccounts<T>(name: string): Promise<Array<{ publicKey: PublicKey; account: T }>> {
    const client = (
      this.program as unknown as {
        account: Record<string, { all(): Promise<Array<{ publicKey: PublicKey; account: T }>> }>;
      }
    ).account[name];
    if (!client) {
      throw new Error(`Unknown Dusk account type: ${name}`);
    }
    return client.all();
  }

  market(account: AddressLike): Promise<Market> {
    return this.programAccount<Market>("market", account);
  }

  borrowPosition(account: AddressLike): Promise<BorrowPosition> {
    return this.programAccount<BorrowPosition>("borrowPosition", account);
  }

  leveragePosition(account: AddressLike): Promise<LeveragePosition> {
    return this.programAccount<LeveragePosition>("leveragePosition", account);
  }

  leverageDelegation(account: AddressLike): Promise<LeverageDelegation> {
    return this.programAccount<LeverageDelegation>("leverageDelegation", account);
  }

  yieldAccount(account: AddressLike): Promise<YieldAccount> {
    return this.programAccount<YieldAccount>("yieldAccount", account);
  }

  futarchyAuthority(account: AddressLike = deriveFutarchyAuthorityAddress()[0]): Promise<FutarchyAuthority> {
    return this.programAccount<FutarchyAuthority>("futarchyAuthority", account);
  }

  referralPartner(account: AddressLike): Promise<ReferralPartner> {
    return this.programAccount<ReferralPartner>("referralPartner", account);
  }

  referralAccrual(account: AddressLike): Promise<ReferralAccrual> {
    return this.programAccount<ReferralAccrual>("referralAccrual", account);
  }

  parameterProposal(account: AddressLike): Promise<ParameterProposal> {
    return this.programAccount<ParameterProposal>("parameterProposal", account);
  }

  proposalSupport(account: AddressLike): Promise<ProposalSupport> {
    return this.programAccount<ProposalSupport>("proposalSupport", account);
  }

  parameterProposalFor(
    market: AddressLike,
    proposer: AddressLike,
    nonce: U64SeedLike
  ): Promise<ParameterProposal> {
    const [proposal] = deriveParameterProposalAddress(
      address(market),
      address(proposer),
      nonce,
      this.program.programId
    );
    return this.parameterProposal(proposal);
  }

  proposalSupportFor(
    proposal: AddressLike,
    supporter: AddressLike
  ): Promise<ProposalSupport> {
    const [support] = deriveProposalSupportAddress(
      address(proposal),
      address(supporter),
      this.program.programId
    );
    return this.proposalSupport(support);
  }

  allMarkets() {
    return this.allProgramAccounts<Market>("market");
  }

  allBorrowPositions() {
    return this.allProgramAccounts<BorrowPosition>("borrowPosition");
  }

  allLeveragePositions() {
    return this.allProgramAccounts<LeveragePosition>("leveragePosition");
  }

  allReferralPartners() {
    return this.allProgramAccounts<ReferralPartner>("referralPartner");
  }

  allReferralAccruals() {
    return this.allProgramAccounts<ReferralAccrual>("referralAccrual");
  }

  allParameterProposals() {
    return this.allProgramAccounts<ParameterProposal>("parameterProposal");
  }

  allProposalSupports() {
    return this.allProgramAccounts<ProposalSupport>("proposalSupport");
  }

  private previewInstruction(
    name: string,
    args: unknown[],
    accounts: Record<string, unknown>
  ): Promise<TransactionInstruction> {
    const methods = (this.program as unknown as {
      methods: Record<
        string,
        (...args: unknown[]) => {
          accounts(accounts: Record<string, unknown>): {
            instruction(): Promise<TransactionInstruction>;
          };
        }
      >;
    }).methods;
    const method = methods[name];
    if (!method) throw new Error(`Unknown Dusk preview instruction: ${name}`);
    return method(...args).accounts(accounts).instruction();
  }

  async previewMarket(market: AddressLike, options: SimulateOptions = {}): Promise<MarketPreview> {
    const instruction = await this.previewInstruction("previewMarket", [], normalizeAccountKeys({ market }));

    return decodePreviewMarketReturnData(await this.simulateReturnData(instruction, options));
  }

  async previewAddLiquidity(params: PreviewAddLiquidityParams): Promise<AddLiquidityPreview> {
    const instruction = await this.previewInstruction(
      "previewAddLiquidity",
      [{
        baseDepositAmount: params.baseDepositAmount,
        quoteDepositAmount: params.quoteDepositAmount,
      }],
      normalizeAccountKeys({
          market: params.market,
          baseMint: params.baseMint,
          quoteMint: params.quoteMint,
      })
    );

    return decodePreviewAddLiquidityReturnData(
      await this.simulateReturnData(instruction, params)
    );
  }

  async previewSwap(params: PreviewSwapParams): Promise<SwapPreview> {
    const instruction = await this.previewInstruction(
      "previewSwap",
      [{
        exactAssetIn: params.exactAssetIn,
      }],
      normalizeAccountKeys({
          market: params.market,
          futarchyAuthority:
            params.futarchyAuthority ?? deriveFutarchyAuthorityAddress()[0],
          assetInMint: params.assetInMint,
          assetOutMint: params.assetOutMint,
      })
    );

    return decodePreviewSwapReturnData(await this.simulateReturnData(instruction, params));
  }

  async previewBorrowCapacity(
    params: PreviewBorrowCapacityParams
  ): Promise<BorrowCapacityPreview> {
    const instruction = await this.previewInstruction(
      "previewBorrowCapacity",
      [{
        collateralAmount: params.collateralAmount,
        projectedBorrowAmount: params.projectedBorrowAmount ?? null,
      }],
      normalizeAccountKeys({
          market: params.market,
          collateralAssetMint: params.collateralAssetMint,
          debtAssetMint: params.debtAssetMint,
      })
    );

    return decodePreviewBorrowCapacityReturnData(await this.simulateReturnData(instruction, params));
  }

  async previewBorrowPositionCapacity(
    params: PreviewBorrowPositionCapacityParams
  ): Promise<BorrowPositionCapacityPreview> {
    const instruction = await this.previewInstruction(
      "previewBorrowPositionCapacity",
      [{
        capacityKind: params.capacityKind === "borrow" ? { borrow: {} } : { withdraw: {} },
        collateralChange: params.collateralChange,
        projectedBorrowAmount: params.projectedBorrowAmount ?? null,
      }],
      normalizeAccountKeys({
          market: params.market,
          borrowPosition: params.borrowPosition,
          collateralAssetMint: params.collateralAssetMint,
          debtAssetMint: params.debtAssetMint,
      })
    );

    return decodePreviewBorrowPositionCapacityReturnData(await this.simulateReturnData(instruction, params));
  }

  async previewBorrowPosition(params: PreviewBorrowPositionParams): Promise<BorrowPositionPreview> {
    const instruction = await this.previewInstruction(
      "previewBorrowPosition",
      [],
      normalizeAccountKeys({
          market: params.market,
          borrowPosition: params.borrowPosition,
      })
    );

    return decodePreviewBorrowPositionReturnData(await this.simulateReturnData(instruction, params));
  }

  /** Simulate one or more instructions while retaining bank and account provenance. */
  simulateWithContext(
    instructions: readonly TransactionInstruction[],
    options: PreviewSimulationOptions = {}
  ) {
    return simulatePreviewWithContext(this.program, instructions, {
      ...options,
      feePayer: options.feePayer ?? this.program.provider.publicKey ?? this.defaultFeePayer,
    });
  }

  async simulateReturnData(
    instruction: TransactionInstruction,
    options: SimulateOptions = {}
  ): Promise<PreviewReturnData> {
    return (await this.simulateWithContext([instruction], options)).value.returnData!;
  }

  previewVirtualBookSnapshot(market: AddressLike, options: PreviewSimulationOptions = {}) {
    return previewVirtualBookSnapshot(this.program, market, options);
  }

  previewVirtualBookBatch(
    snapshot: DuskVirtualBookSnapshot,
    requests: VirtualBookQuoteRequest[],
    options: PreviewSimulationOptions = {}
  ) {
    return previewVirtualBookBatch(this.program, snapshot, requests, options);
  }

  previewVirtualBookQuotes(
    snapshot: DuskVirtualBookSnapshot,
    options: VirtualBookQuoteOptions = {}
  ) {
    return previewVirtualBookQuotes(this.program, snapshot, options);
  }

  /** Fee-inclusive display depth. Transaction quotes must still use the exact requested input. */
  async previewVirtualBook(market: AddressLike, options: VirtualBookQuoteOptions = {}) {
    const snapshot = await this.previewVirtualBookSnapshot(market, options);
    const quotes = await this.previewVirtualBookQuotes(snapshot, options);
    return { ...quotes, book: projectDuskVirtualBook(quotes)! };
  }
}
