import type { BN, Program } from "@coral-xyz/anchor";
import {
  PublicKey,
  Transaction,
  type Commitment,
  type TransactionInstruction,
} from "@solana/web3.js";

import {
  deriveBorrowPositionAddress,
  deriveFutarchyAuthorityAddress,
  deriveHlpYlpVaultAddress,
  deriveInsuranceAddress,
  deriveLeveragePositionAddress,
  deriveMarketAddress,
  deriveMarketCollateralVaultAddress,
  deriveMarketFeeVaultAddress,
  deriveMarketInterestVaultAddress,
  deriveMarketReserveVaultAddress,
  deriveReferralAccrualAddress,
  deriveReferralProfileAddress,
  deriveTokenMetadataAddress,
  deriveYieldAccountAddress,
  deriveYieldTransferHookValidationAddress,
} from "./constants.js";
import { address, DEFAULT_READONLY_PUBLIC_KEY, normalizeAccountKeys, type AddressLike } from "./address.js";
import {
  decodePreviewAddLiquidityReturnData,
  decodePreviewBorrowCapacityReturnData,
  decodePreviewBorrowPositionReturnData,
  decodePreviewMarketReturnData,
  decodePreviewSwapReturnData,
  type AddLiquidityPreview,
  type BorrowCapacityPreview,
  type BorrowPositionPreview,
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
  ReferralAccrual,
  ReferralProfile,
  YieldAccount,
} from "./type-aliases.js";
import type { Dusk } from "./types_v2.js";

export const pda = {
  futarchyAuthority: deriveFutarchyAuthorityAddress,
  market: deriveMarketAddress,
  tokenMetadata: deriveTokenMetadataAddress,
  marketReserveVault: deriveMarketReserveVaultAddress,
  marketCollateralVault: deriveMarketCollateralVaultAddress,
  marketFeeVault: deriveMarketFeeVaultAddress,
  marketInterestVault: deriveMarketInterestVaultAddress,
  borrowPosition: deriveBorrowPositionAddress,
  leveragePosition: deriveLeveragePositionAddress,
  yieldAccount: deriveYieldAccountAddress,
  yieldTransferHookValidation: deriveYieldTransferHookValidationAddress,
  hlpYlpVault: deriveHlpYlpVaultAddress,
  insurance: deriveInsuranceAddress,
  referralProfile: deriveReferralProfileAddress,
  referralAccrual: deriveReferralAccrualAddress,
} as const;

export interface SimulateOptions {
  feePayer?: AddressLike;
  commitment?: Commitment;
}

export interface PreviewSwapParams extends SimulateOptions {
  market: AddressLike;
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

export interface PreviewBorrowPositionParams extends SimulateOptions {
  market: AddressLike;
  borrowPosition: AddressLike;
}

export class DuskSimulationError extends Error {
  constructor(
    message: string,
    readonly simulation: Awaited<ReturnType<Program<Dusk>["provider"]["connection"]["simulateTransaction"]>>
  ) {
    super(message);
    this.name = "DuskSimulationError";
  }
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

  leverageDelegation(account: AddressLike): Promise<LeverageDelegation> {
    return this.program.account.leverageDelegation.fetch(address(account));
  }

  yieldAccount(account: AddressLike): Promise<YieldAccount> {
    return this.program.account.yieldAccount.fetch(address(account));
  }

  futarchyAuthority(account: AddressLike = deriveFutarchyAuthorityAddress()[0]): Promise<FutarchyAuthority> {
    return this.program.account.futarchyAuthority.fetch(address(account));
  }

  referralProfile(account: AddressLike): Promise<ReferralProfile> {
    return this.program.account.referralProfile.fetch(address(account));
  }

  referralAccrual(account: AddressLike): Promise<ReferralAccrual> {
    return this.program.account.referralAccrual.fetch(address(account));
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

  allReferralProfiles() {
    return this.program.account.referralProfile.all();
  }

  allReferralAccruals() {
    return this.program.account.referralAccrual.all();
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

  async simulateReturnData(
    instruction: TransactionInstruction,
    options: SimulateOptions = {}
  ): Promise<PreviewReturnData> {
    const tx = new Transaction().add(instruction);
    tx.feePayer = address(options.feePayer ?? this.program.provider.publicKey ?? this.defaultFeePayer);
    tx.recentBlockhash = (
      await this.program.provider.connection.getLatestBlockhash(options.commitment)
    ).blockhash;

    const simulation = await this.program.provider.connection.simulateTransaction(tx);
    if (simulation.value.err) {
      throw new DuskSimulationError("Dusk simulation failed", simulation);
    }
    if (!simulation.value.returnData) {
      throw new DuskSimulationError("Dusk simulation did not return data", simulation);
    }
    if (simulation.value.returnData.programId !== this.program.programId.toBase58()) {
      throw new DuskSimulationError("Dusk simulation returned data from a different program", simulation);
    }
    return simulation.value.returnData;
  }
}
