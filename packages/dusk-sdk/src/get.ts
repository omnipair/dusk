import type { BN, Program } from "@coral-xyz/anchor";
import { PublicKey, TransactionInstruction, type Commitment } from "@solana/web3.js";
import {
  DuskSimulationError,
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
import { governanceIntegerBN } from "./governance.js";
import { findMinimumNativeCloseCollateralIn } from "./native-close.js";
import { DuskWrite, type CloseLeverageParams, type RawAmount } from "./write.js";
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

export interface FindCollateralLeverageCloseInputParams
  extends Omit<CloseLeverageParams, "collateralFunded" | "collateralIn"> {
  /** Optional sale cap; the minimum payout also caps how much can be sold. */
  maxCollateralIn?: RawAmount;
  simulation?: Omit<PreviewSimulationOptions, "accounts" | "requireReturnData">;
}

export interface CollateralLeverageCloseInput {
  collateralIn: bigint;
  collateralReturned: bigint;
  /** Slot of the final successful simulation, not a guarantee for a later bank. */
  observedSlot: number;
}

function closeSimulationErrorCode(error: unknown): number | undefined {
  if (!(error instanceof DuskSimulationError)) return undefined;
  const instructionError = (error.simulation.value.err as {
    InstructionError?: [number, { Custom?: number }];
  } | null)?.InstructionError;
  // The two compute-budget instructions precede the close instruction.
  return instructionError?.[0] === 2 ? instructionError[1]?.Custom : undefined;
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

  market(account: AddressLike): Promise<Market> {
    return this.program.account.market.fetch(address(account));
  }

  borrowPosition(account: AddressLike): Promise<BorrowPosition> {
    return this.program.account.borrowPosition.fetch(address(account));
  }

  leveragePosition(account: AddressLike): Promise<LeveragePosition> {
    return this.program.account.leveragePosition.fetch(address(account));
  }

  /**
   * Find the smallest collateral sale that completes the native close. Each
   * candidate simulates the actual close instruction, so interest, swap fees,
   * hLP settlement, and the owner's payout floor use the on-chain path.
   * Rebuild and submit the close promptly; state can change after simulation.
   */
  async findCollateralLeverageCloseInput(
    params: FindCollateralLeverageCloseInputParams
  ): Promise<CollateralLeverageCloseInput> {
    const { maxCollateralIn, simulation, ...closeParams } = params;
    const marketKey = address(closeParams.market);
    const positionKey = address(closeParams.leveragePosition ??
      deriveLeveragePositionAddress(marketKey, address(closeParams.positionId))[0]);
    const [position, market] = await Promise.all([
      this.leveragePosition(positionKey),
      this.market(marketKey),
    ]);
    const debtAsset = closeParams.debtAsset === "base" ? 0 : closeParams.debtAsset === "quote" ? 1 : -1;
    if (debtAsset < 0 || position.debtAsset !== debtAsset ||
      !position.owner.equals(address(closeParams.positionOwner)) ||
      !position.market.equals(marketKey) ||
      !position.positionId.equals(address(closeParams.positionId)) ||
      BigInt(position.fundedCollateralAmount.toString()) === 0n)
      throw new Error("The requested position is not a matching native collateral position");
    const total = BigInt(position.collateralAmount.toString());
    const minimumReturned = BigInt(governanceIntegerBN(closeParams.minAmountOut, "minAmountOut").toString());
    if (minimumReturned >= total)
      throw new Error("Minimum collateral payout leaves no amount available for repayment");
    const available = total - minimumReturned;
    const requestedCap = maxCollateralIn === undefined
      ? available
      : BigInt(governanceIntegerBN(maxCollateralIn, "maxCollateralIn").toString());
    const maximum = requestedCap < available ? requestedCap : available;
    if (maximum === 0n) throw new Error("No collateral is available for repayment");

    // Build account metas once. Only the encoded exact input changes per probe.
    const baseInstruction = await new DuskWrite(this.program).closeLeverageInstruction({
      ...closeParams,
      collateralFunded: true,
      collateralIn: maximum,
    });
    const candidateInstruction = (amount: bigint) => new TransactionInstruction({
      programId: baseInstruction.programId,
      keys: baseInstruction.keys,
      data: this.program.coder.instruction.encode("closeCollateralLeverage", {
        args: {
          debtAsset,
          collateralIn: governanceIntegerBN(amount.toString()),
          minCollateralOut: governanceIntegerBN(minimumReturned.toString()),
        },
      }),
    });
    const insufficientCodes = new Set<number>(
      this.program.idl.errors
        .filter((entry) => ["insufficientamount", "insufficientoutputamount"].includes(entry.name.toLowerCase()))
        .map((entry) => entry.code)
    );
    const insufficientLiquidityCode = this.program.idl.errors
      .find((entry) => entry.name.toLowerCase() === "insufficientliquidity")?.code;
    const probe = async (amount: bigint) => {
      try {
        const result = await this.simulateWithContext([candidateInstruction(amount)], {
          ...simulation,
          feePayer: simulation?.feePayer ?? closeParams.authority ?? closeParams.positionOwner,
          requireReturnData: false,
        });
        return { status: "sufficient" as const, slot: result.context.slot };
      } catch (error) {
        const code = closeSimulationErrorCode(error);
        if (code !== undefined && insufficientCodes.has(code))
          return { status: "insufficient" as const, slot: 0 };
        if (code !== undefined && code === insufficientLiquidityCode)
          return { status: "liquidity-limited" as const, slot: 0 };
        throw error;
      }
    };
    const amm = market.config.amm;
    const feeTiers = amm.launchRateLimitAsset === debtAsset + 1 &&
      amm.launchRateLimitMaxFeeBps > 0
      ? {
          referenceNad: BigInt(amm.launchRateLimitReferenceNad.toString()),
          incrementBps: amm.launchRateLimitIncrementBps,
          maxFeeBps: amm.launchRateLimitMaxFeeBps,
          collateralDecimals: debtAsset === 0
            ? market.quoteSide.assetDecimals : market.baseSide.assetDecimals,
        }
      : undefined;
    const collateralIn = await findMinimumNativeCloseCollateralIn({
      maxCollateralIn: maximum,
      feeTiers,
      canClose: async (amount) => {
        const result = await probe(amount);
        return result.status === "liquidity-limited" ? result.status : result.status === "sufficient";
      },
      signal: simulation?.signal,
    });
    // One fresh simulation catches a market change during the search.
    const final = await probe(collateralIn);
    if (final.status !== "sufficient") throw new Error("Market changed during the close quote; retry");
    return {
      collateralIn,
      collateralReturned: total - collateralIn,
      observedSlot: final.slot,
    };
  }

  leverageDelegation(account: AddressLike): Promise<LeverageDelegation> {
    return this.program.account.leverageDelegation.fetch(address(account));
  }

  yieldAccount(account: AddressLike): Promise<YieldAccount> {
    return this.program.account.yieldAccount.fetch(address(account));
  }

  futarchyAuthority(account: AddressLike = deriveFutarchyAuthorityAddress()[0]): Promise<FutarchyAuthority> {
    return this.program.account.futarchyAuthority.fetch(address(account));
  }

  referralPartner(account: AddressLike): Promise<ReferralPartner> {
    return this.program.account.referralPartner.fetch(address(account));
  }

  referralAccrual(account: AddressLike): Promise<ReferralAccrual> {
    return this.program.account.referralAccrual.fetch(address(account));
  }

  parameterProposal(account: AddressLike): Promise<ParameterProposal> {
    return this.program.account.parameterProposal.fetch(address(account));
  }

  proposalSupport(account: AddressLike): Promise<ProposalSupport> {
    return this.program.account.proposalSupport.fetch(address(account));
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
    return this.program.account.market.all();
  }

  allBorrowPositions() {
    return this.program.account.borrowPosition.all();
  }

  allLeveragePositions() {
    return this.program.account.leveragePosition.all();
  }

  allReferralPartners() {
    return this.program.account.referralPartner.all();
  }

  allReferralAccruals() {
    return this.program.account.referralAccrual.all();
  }

  allParameterProposals() {
    return this.program.account.parameterProposal.all();
  }

  allProposalSupports() {
    return this.program.account.proposalSupport.all();
  }

  async previewMarket(market: AddressLike, options: SimulateOptions = {}): Promise<MarketPreview> {
    const instruction = await this.program.methods
      .previewMarket()
      .accounts(normalizeAccountKeys({ market }))
      .instruction();

    return decodePreviewMarketReturnData(await this.simulateReturnData(instruction, options));
  }

  async previewAddLiquidity(params: PreviewAddLiquidityParams): Promise<AddLiquidityPreview> {
    const instruction = await this.program.methods
      .previewAddLiquidity({
        baseDepositAmount: params.baseDepositAmount,
        quoteDepositAmount: params.quoteDepositAmount,
      })
      .accounts(
        normalizeAccountKeys({
          market: params.market,
          baseMint: params.baseMint,
          quoteMint: params.quoteMint,
        })
      )
      .instruction();

    return decodePreviewAddLiquidityReturnData(
      await this.simulateReturnData(instruction, params)
    );
  }

  async previewSwap(params: PreviewSwapParams): Promise<SwapPreview> {
    const instruction = await this.program.methods
      .previewSwap({
        exactAssetIn: params.exactAssetIn,
      })
      .accounts(
        normalizeAccountKeys({
          market: params.market,
          futarchyAuthority:
            params.futarchyAuthority ?? deriveFutarchyAuthorityAddress()[0],
          assetInMint: params.assetInMint,
          assetOutMint: params.assetOutMint,
        })
      )
      .instruction();

    return decodePreviewSwapReturnData(await this.simulateReturnData(instruction, params));
  }

  async previewBorrowCapacity(
    params: PreviewBorrowCapacityParams
  ): Promise<BorrowCapacityPreview> {
    const instruction = await this.program.methods
      .previewBorrowCapacity({
        collateralAmount: params.collateralAmount,
        projectedBorrowAmount: params.projectedBorrowAmount ?? null,
      })
      .accounts(
        normalizeAccountKeys({
          market: params.market,
          collateralAssetMint: params.collateralAssetMint,
          debtAssetMint: params.debtAssetMint,
        })
      )
      .instruction();

    return decodePreviewBorrowCapacityReturnData(await this.simulateReturnData(instruction, params));
  }

  async previewBorrowPositionCapacity(
    params: PreviewBorrowPositionCapacityParams
  ): Promise<BorrowPositionCapacityPreview> {
    const instruction = await this.program.methods
      .previewBorrowPositionCapacity({
        capacityKind: params.capacityKind === "borrow" ? { borrow: {} } : { withdraw: {} },
        collateralChange: params.collateralChange,
        projectedBorrowAmount: params.projectedBorrowAmount ?? null,
      })
      .accounts(
        normalizeAccountKeys({
          market: params.market,
          borrowPosition: params.borrowPosition,
          collateralAssetMint: params.collateralAssetMint,
          debtAssetMint: params.debtAssetMint,
        })
      )
      .instruction();

    return decodePreviewBorrowPositionCapacityReturnData(await this.simulateReturnData(instruction, params));
  }

  async previewBorrowPosition(params: PreviewBorrowPositionParams): Promise<BorrowPositionPreview> {
    const instruction = await this.program.methods
      .previewBorrowPosition()
      .accounts(
        normalizeAccountKeys({
          market: params.market,
          borrowPosition: params.borrowPosition,
        })
      )
      .instruction();

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
