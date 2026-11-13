import type { DuskInstructionName } from "./instruction-args.js";
import { DuskWrite, type DuskBuildCall } from "./write.js";
import type { BN, Program } from "@coral-xyz/anchor";
import { previewLeverageAction, type PreviewLeverageActionParams } from "./leverage-preview.js";
import { PublicKey, TransactionInstruction, type Commitment } from "@solana/web3.js";
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
  deriveHlpSrlpVaultAddress,
  deriveLeveragePositionAddress,
  deriveMarketAddress,
  deriveMarketCollateralVaultAddress,
  deriveMarketInterestVaultAddress,
  deriveMarketReserveVaultAddress,
  deriveReferralAccrualAddress,
  deriveReferralPartnerAddress,
  deriveTokenMetadataAddress,
  deriveYieldAccountAddress,
  deriveYieldTransferHookValidationAddress,
  DUSK_PROGRAM_ID,
  type U64SeedLike,
  type YieldTokenKind,
} from "./constants.js";
import { address, DEFAULT_READONLY_PUBLIC_KEY, normalizeAccountKeys, type AddressLike } from "./address.js";
import { getStoredLeverageMargins } from "./leverage-margins.js";
import { assertCurrentMarketLayout } from "./market-layout.js";
import {
  decodePreviewHlpDepositCapacityReturnData,
  type HlpDepositCapacityPreview,
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
  ReferralAccrual,
  ReferralPartner,
  YieldAccount,
} from "./type-aliases.js";
import type { Dusk } from "./types_v2.js";

/**
 * The Dusk PDA helpers for one deployment. Each takes the same arguments as
 * its `derive*Address` function; an omitted trailing program ID is
 * `programId` rather than `DUSK_PROGRAM_ID`. `tokenMetadata` is a Metaplex
 * address and does not depend on the Dusk deployment.
 */
export function duskPdas(programId: PublicKey = DUSK_PROGRAM_ID) {
  return {
    futarchyAuthority: (id: PublicKey = programId) => deriveFutarchyAuthorityAddress(id),
    market: (baseMint: PublicKey, quoteMint: PublicKey, paramsHash: Uint8Array | Buffer | number[], id: PublicKey = programId) =>
      deriveMarketAddress(baseMint, quoteMint, paramsHash, id),
    tokenMetadata: deriveTokenMetadataAddress,
    marketReserveVault: (market: PublicKey, reserveMint: PublicKey, id: PublicKey = programId) =>
      deriveMarketReserveVaultAddress(market, reserveMint, id),
    marketCollateralVault: (market: PublicKey, collateralMint: PublicKey, id: PublicKey = programId) =>
      deriveMarketCollateralVaultAddress(market, collateralMint, id),
    marketInterestVault: (market: PublicKey, interestMint: PublicKey, id: PublicKey = programId) =>
      deriveMarketInterestVaultAddress(market, interestMint, id),
    borrowPosition: (market: PublicKey, owner: PublicKey, positionId: PublicKey, id: PublicKey = programId) =>
      deriveBorrowPositionAddress(market, owner, positionId, id),
    leveragePosition: (market: PublicKey, positionOwner: PublicKey, positionId: PublicKey,
      namespaceAuthority: PublicKey = positionOwner, id: PublicKey = programId) =>
      deriveLeveragePositionAddress(market, positionOwner, positionId, namespaceAuthority, id),
    yieldAccount: (market: PublicKey, owner: PublicKey, lpMint: PublicKey, assetMint: PublicKey,
      tokenKind: YieldTokenKind, id: PublicKey = programId) =>
      deriveYieldAccountAddress(market, owner, lpMint, assetMint, tokenKind, id),
    yieldTransferHookValidation: (lpMint: PublicKey, id: PublicKey = programId) =>
      deriveYieldTransferHookValidationAddress(lpMint, id),
    hlpSrlpVault: (market: PublicKey, targetHlpMint: PublicKey, srlpMint: PublicKey, id: PublicKey = programId) =>
      deriveHlpSrlpVaultAddress(market, targetHlpMint, srlpMint, id),
    referralPartner: (authority: PublicKey, id: PublicKey = programId) => deriveReferralPartnerAddress(authority, id),
    referralAccrual: (referralPartner: PublicKey, market: PublicKey, assetMint: PublicKey, id: PublicKey = programId) =>
      deriveReferralAccrualAddress(referralPartner, market, assetMint, id),
  } as const;
}

export type DuskPdas = ReturnType<typeof duskPdas>;

/** PDA helpers for the canonical deployment, `DUSK_PROGRAM_ID`. A `DuskGet`
 * carries its own program's helpers as `get.pda`. */
export const pda: DuskPdas = duskPdas();

export interface PreviewHlpDepositCapacityParams extends SimulateOptions {
  market: AddressLike;
  futarchyAuthority?: AddressLike;
  baseMint: AddressLike;
  quoteMint: AddressLike;
  targetHlpMint: AddressLike;
}

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
  /** Net collateral amount credited to the vault after any deposit transfer fee. */
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
  /** Market base mint; liquidation fields value collateral after its transfer fee. */
  baseMint: AddressLike;
  /** Market quote mint; liquidation fields value collateral after its transfer fee. */
  quoteMint: AddressLike;
}

export class DuskGet {
  /** PDA helpers that derive under this client's program ID. */
  readonly pda: DuskPdas;

  constructor(
    readonly program: Program<Dusk>,
    private readonly defaultFeePayer: AddressLike = DEFAULT_READONLY_PUBLIC_KEY
  ) {
    this.pda = duskPdas(program.programId);
  }

  /** Raw account bytes, returned without decoding or a layout check. */
  async accountInfo(account: AddressLike, commitment?: Commitment) {
    return this.program.provider.connection.getAccountInfo(address(account), commitment);
  }

  /** Decodes one program account by its IDL account name. A `market` account
   * at any layout other than `MARKET_LAYOUT_VERSION` throws
   * `DuskUnsupportedMarketLayoutError`. */
  async programAccount<T = unknown>(name: string, account: AddressLike): Promise<T> {
    const client = (
      this.program.account as unknown as Record<string, { fetch(address: PublicKey): Promise<T> }>
    )[name];
    if (!client) {
      throw new Error(`Unknown Dusk account type: ${name}`);
    }
    const decoded = await client.fetch(address(account));
    if (name === "market") {
      assertCurrentMarketLayout(decoded as Pick<Market, "version">);
    }
    return decoded;
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

  /** Throws `DuskUnsupportedMarketLayoutError` for a market at any layout
   * other than `MARKET_LAYOUT_VERSION`. */
  market(account: AddressLike): Promise<Market> {
    return this.programAccount<Market>("market", account);
  }

  borrowPosition(account: AddressLike): Promise<BorrowPosition> {
    return this.programAccount<BorrowPosition>("borrowPosition", account);
  }

  leveragePosition(account: AddressLike): Promise<LeveragePosition> {
    return this.programAccount<LeveragePosition>("leveragePosition", account);
  }

  /** Fetch saved margin terms; does not quote an opening or an increase. */
  async leverageMargins(account: AddressLike) {
    return getStoredLeverageMargins(await this.leveragePosition(account));
  }

  previewLeverageAction(params: PreviewLeverageActionParams) {
    return previewLeverageAction(this.program, params);
  }

  leverageDelegation(account: AddressLike): Promise<LeverageDelegation> {
    return this.programAccount<LeverageDelegation>("leverageDelegation", account);
  }

  yieldAccount(account: AddressLike): Promise<YieldAccount> {
    return this.programAccount<YieldAccount>("yieldAccount", account);
  }

  futarchyAuthority(
    account: AddressLike = deriveFutarchyAuthorityAddress(this.program.programId)[0]
  ): Promise<FutarchyAuthority> {
    return this.programAccount<FutarchyAuthority>("futarchyAuthority", account);
  }

  referralPartner(account: AddressLike): Promise<ReferralPartner> {
    return this.programAccount<ReferralPartner>("referralPartner", account);
  }

  referralAccrual(account: AddressLike): Promise<ReferralAccrual> {
    return this.programAccount<ReferralAccrual>("referralAccrual", account);
  }

  /** Every market account. Throws `DuskUnsupportedMarketLayoutError` if any
   * is at a layout other than `MARKET_LAYOUT_VERSION`, rather than returning
   * it as a current market. */
  async allMarkets(): Promise<Array<{ publicKey: PublicKey; account: Market }>> {
    const markets = await this.allProgramAccounts<Market>("market");
    for (const { account } of markets) {
      assertCurrentMarketLayout(account);
    }
    return markets;
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

  private previewInstruction<N extends DuskInstructionName>(
    name: N,
    ...call: DuskBuildCall<N>
  ): Promise<TransactionInstruction> {
    return new DuskWrite(this.program).instruction(name, ...call);
  }

  async previewMarket(market: AddressLike, options: SimulateOptions = {}): Promise<MarketPreview> {
    const instruction = await this.previewInstruction("previewMarket", [], { accounts: normalizeAccountKeys({ market }) });

    return decodePreviewMarketReturnData(await this.simulateReturnData(instruction, options));
  }

  /** Current full-value hedge admission and funding ceiling, including transfer fees.
   * A Ready status is not an amount-specific execution guarantee; simulate the
   * completed deposit transaction to check rounding, slippage and custody.
   */
  async previewHlpDepositCapacity(params: PreviewHlpDepositCapacityParams): Promise<HlpDepositCapacityPreview> {
    const instruction = await this.previewInstruction("previewHlpDepositCapacity", [], { accounts: normalizeAccountKeys({
      market: params.market,
      futarchyAuthority: params.futarchyAuthority ?? deriveFutarchyAuthorityAddress(this.program.programId)[0],
      baseMint: params.baseMint,
      quoteMint: params.quoteMint,
      targetHlpMint: params.targetHlpMint,
    }) });
    return decodePreviewHlpDepositCapacityReturnData(await this.simulateReturnData(instruction, params));
  }

  async previewAddLiquidity(params: PreviewAddLiquidityParams): Promise<AddLiquidityPreview> {
    const instruction = await this.previewInstruction(
      "previewAddLiquidity",
      [{
        baseDepositAmount: params.baseDepositAmount,
        quoteDepositAmount: params.quoteDepositAmount,
      }],
      { accounts: normalizeAccountKeys({
          market: params.market,
          baseMint: params.baseMint,
          quoteMint: params.quoteMint,
      }) }
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
      { accounts: normalizeAccountKeys({
          market: params.market,
          futarchyAuthority:
            params.futarchyAuthority ?? deriveFutarchyAuthorityAddress(this.program.programId)[0],
          assetInMint: params.assetInMint,
          assetOutMint: params.assetOutMint,
      }) }
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
      { accounts: normalizeAccountKeys({
          market: params.market,
          collateralAssetMint: params.collateralAssetMint,
          debtAssetMint: params.debtAssetMint,
      }) }
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
      { accounts: normalizeAccountKeys({
          market: params.market,
          borrowPosition: params.borrowPosition,
          collateralAssetMint: params.collateralAssetMint,
          debtAssetMint: params.debtAssetMint,
      }) }
    );

    return decodePreviewBorrowPositionCapacityReturnData(await this.simulateReturnData(instruction, params));
  }

  async previewBorrowPosition(params: PreviewBorrowPositionParams): Promise<BorrowPositionPreview> {
    const instruction = await this.previewInstruction(
      "previewBorrowPosition",
      [],
      { accounts: normalizeAccountKeys({
          market: params.market,
          borrowPosition: params.borrowPosition,
          baseMint: params.baseMint,
          quoteMint: params.quoteMint,
      }) }
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
