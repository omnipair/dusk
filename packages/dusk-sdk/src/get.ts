import type { BN, Program } from "@coral-xyz/anchor";
import { PublicKey, SYSVAR_CLOCK_PUBKEY, TransactionInstruction, type AccountInfo, type Commitment } from "@solana/web3.js";
import { getEpochFee, getTransferFeeConfig, TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, unpackMint } from "@solana/spl-token";
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
import { governanceIntegerBN } from "./governance.js";
import { findMinimumNativeCloseCollateralIn, nativeCloseCredit, nativeCloseGrossForCredit,
  type NativeCloseTransferFee } from "./native-close.js";
import { createNativeCloseQuote } from "./native-close-quote.js";
import { DuskWrite, type CloseLeverageParams, type RawAmount } from "./write.js";
import { getStoredLeverageMargins } from "./leverage-margins.js";
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

export interface FindCollateralLeverageCloseInputParams
  extends Omit<CloseLeverageParams, "collateralFunded" | "collateralIn"> {
  /** Optional sale cap; the minimum payout also caps how much can be sold. */
  maxCollateralIn?: RawAmount;
  simulation?: Omit<PreviewSimulationOptions, "accounts" | "requireReturnData">;
}

export interface CollateralLeverageCloseInput {
  collateralIn: bigint;
  /** Net collateral payout after its transfer fee, at the final simulation bank. */
  collateralReturned: bigint;
  /** Slot of the final successful simulation, not a guarantee for a later bank. */
  observedSlot: number;
}

function nativeCloseFeeFromAccounts(
  mintKey: PublicKey, mint: AccountInfo<Buffer> | null, clock: AccountInfo<Buffer> | null
): NativeCloseTransferFee | undefined {
  if (!mint || !clock || mint.executable || clock.executable ||
    (!mint.owner.equals(TOKEN_PROGRAM_ID) && !mint.owner.equals(TOKEN_2022_PROGRAM_ID)) ||
    clock.owner.toBase58() !== "Sysvar1111111111111111111111111111111111111" || clock.data.length !== 40)
    throw new Error("Invalid native close mint/clock snapshot");
  const config = getTransferFeeConfig(unpackMint(mintKey, mint, mint.owner));
  return config ? getEpochFee(config, clock.data.readBigUInt64LE(16)) : undefined;
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

  /** Fetch saved margin terms; does not quote an opening or an increase. */
  async leverageMargins(account: AddressLike) {
    return getStoredLeverageMargins(await this.leveragePosition(account));
  }

  /**
   * Price candidate sales locally with the program's Rust transition math and
   * effective collateral transfer fee. Simulate only the selected close to
   * validate account constraints and current market state before submission.
   */
  async findCollateralLeverageCloseInput(
    params: FindCollateralLeverageCloseInputParams
  ): Promise<CollateralLeverageCloseInput> {
    const { maxCollateralIn, simulation, ...closeParams } = params;
    const marketKey = address(closeParams.market);
    const positionKey = address(closeParams.leveragePosition ??
      deriveLeveragePositionAddress(marketKey, address(closeParams.positionOwner),
        address(closeParams.positionId), address(closeParams.namespaceAuthority ?? closeParams.positionOwner))[0]);
    const collateralMintKey = address(closeParams.collateralMint);
    simulation?.signal?.throwIfAborted();
    const snapshot = await this.program.provider.connection.getMultipleAccountsInfoAndContext(
      [marketKey, positionKey, collateralMintKey, SYSVAR_CLOCK_PUBKEY],
      {
        ...(simulation?.commitment === undefined ? {} : { commitment: simulation.commitment }),
        ...(simulation?.minContextSlot === undefined ? {} : { minContextSlot: simulation.minContextSlot }),
      }
    );
    simulation?.signal?.throwIfAborted();
    const [marketAccount, positionAccount, mintAccount, clockAccount] = snapshot.value;
    if (!marketAccount || !positionAccount || !clockAccount ||
      !marketAccount.owner.equals(this.program.programId) ||
      !positionAccount.owner.equals(this.program.programId) ||
      clockAccount.data.length < 40 ||
      clockAccount.data.readBigUInt64LE(0) !== BigInt(snapshot.context.slot))
      throw new Error("Native-close account snapshot is incomplete or inconsistent");
    const market = this.program.coder.accounts.decode("market", marketAccount.data) as Market;
    const position = this.program.coder.accounts.decode("leveragePosition", positionAccount.data) as LeveragePosition;
    const debtAsset = closeParams.debtAsset === "base" ? 0 : closeParams.debtAsset === "quote" ? 1 : -1;
    if (debtAsset < 0 || position.debtAsset !== debtAsset ||
      !position.owner.equals(address(closeParams.positionOwner)) ||
      !position.market.equals(marketKey) ||
      !position.positionId.equals(address(closeParams.positionId)) ||
      BigInt(position.fundedCollateralAmount.toString()) === 0n)
      throw new Error("The requested position is not a matching native collateral position");
    const total = BigInt(position.collateralAmount.toString());
    const transferFee = nativeCloseFeeFromAccounts(collateralMintKey, mintAccount, clockAccount);
    const minimumReturned = BigInt(governanceIntegerBN(closeParams.minAmountOut, "minAmountOut").toString());
    const minimumGrossReturned = nativeCloseGrossForCredit(minimumReturned, transferFee);
    if (minimumGrossReturned >= total)
      throw new Error("Minimum collateral payout leaves no amount available for repayment");
    const available = total - minimumGrossReturned;
    const requestedCap = maxCollateralIn === undefined
      ? available
      : BigInt(governanceIntegerBN(maxCollateralIn, "maxCollateralIn").toString());
    const maximum = requestedCap < available ? requestedCap : available;
    if (maximum === 0n) throw new Error("No collateral is available for repayment");

    const quote = await createNativeCloseQuote(
      marketAccount.data,
      positionAccount.data,
      BigInt(snapshot.context.slot),
      clockAccount.data.readBigInt64LE(32)
    );
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
      transferFee,
      canClose: async (amount) => {
        const output = quote.amountOut(nativeCloseCredit(amount, transferFee));
        return output === "liquidity-limited" ? output :
          output === "insufficient" ? false : output >= quote.debtAmount;
      },
      signal: simulation?.signal,
    });
    const closeInstruction = await new DuskWrite(this.program).closeLeverageInstruction({
      ...closeParams,
      collateralFunded: true,
      collateralIn,
    }, market);
    const final = await this.simulateWithContext([closeInstruction], {
      ...simulation,
      minContextSlot: Math.max(snapshot.context.slot, simulation?.minContextSlot ?? 0),
      feePayer: simulation?.feePayer ?? closeParams.authority ?? closeParams.positionOwner,
      requireReturnData: false,
      accounts: [collateralMintKey, SYSVAR_CLOCK_PUBKEY],
    });
    const finalAccounts = final.value.accounts?.map((info) => info && ({
      ...info, owner: new PublicKey(info.owner), data: Buffer.from(info.data[0], "base64"),
    }));
    const finalFee = nativeCloseFeeFromAccounts(
      collateralMintKey, finalAccounts?.[0] ?? null, finalAccounts?.[1] ?? null
    );
    if ((finalFee?.transferFeeBasisPoints ?? 0) !== (transferFee?.transferFeeBasisPoints ?? 0) ||
      (finalFee?.maximumFee ?? 0n) !== (transferFee?.maximumFee ?? 0n))
      throw new Error("Collateral transfer fee changed during the close quote; retry");
    return {
      collateralIn,
      collateralReturned: nativeCloseCredit(total - collateralIn, finalFee),
      observedSlot: final.context.slot,
    };
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
          baseMint: params.baseMint,
          quoteMint: params.quoteMint,
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
