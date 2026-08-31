import type { Program } from "@coral-xyz/anchor";
import {
  getAssociatedTokenAddressSync,
  getMint,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import {
  SystemProgram,
  SYSVAR_INSTRUCTIONS_PUBKEY,
  Transaction,
  type AccountMeta,
  type PublicKey,
  type TransactionInstruction,
} from "@solana/web3.js";

import { address, normalizeAccountKeys, type AddressLike } from "./address.js";
import {
  deriveBorrowPositionAddress,
  deriveEventAuthorityAddress,
  deriveFutarchyAuthorityAddress,
  deriveLeverageCollateralVaultAddress,
  deriveLeverageDelegationAddress,
  deriveLeveragePositionAddress,
  deriveMarketCollateralVaultAddress,
  deriveMarketInterestVaultAddress,
  deriveMarketReserveVaultAddress,
  deriveReferralAccrualAddress,
  deriveParameterProposalAddress,
  deriveProposalSupportAddress,
  deriveYieldAccountAddress,
  deriveYieldTransferHookValidationAddress,
  deriveReferralPartnerAddress,
  type YieldTokenKind,
} from "./constants.js";
import {
  anchorParameterUpdate,
  assertProposalMetadata,
  governanceIntegerBN,
  type GovernanceIntegerLike,
  type ParameterUpdate,
  type ProposalMetadataV1,
} from "./governance.js";
import {
  assertReferralInterestShareBps,
  referralAccrualAddresses,
  resolveTransferHookAccountMetas,
  tokenProgramForMint,
  type TransferHookTransfer,
} from "./referral.js";
import type { Dusk } from "./types_v2.js";

export type DuskInstructionName = Dusk["instructions"][number]["name"];
export type DuskInstructionArgs = unknown[] | unknown | undefined;
export type DuskAccounts = Record<string, unknown>;

export interface DuskBuildOptions {
  accounts?: DuskAccounts;
  remainingAccounts?: AccountMeta[];
}

export interface SwapBuildOptions extends DuskBuildOptions {
  accounts: DuskAccounts;
  market: AddressLike;
}

export type ReferredActionName = "borrow" | "openLeverage";

export interface ReferredActionOptions extends DuskBuildOptions {
  accounts: DuskAccounts;
  payer: AddressLike;
  referrer: AddressLike;
  market: AddressLike;
  debtMint: AddressLike;
  transferHookTransfers?: readonly TransferHookTransfer[];
}

export interface ReferredActionBuild {
  referralPartner: ReturnType<typeof deriveReferralPartnerAddress>[0];
  referralAccrual: ReturnType<typeof deriveReferralPartnerAddress>[0];
  setupInstruction: TransactionInstruction;
  actionInstruction: TransactionInstruction;
  transaction: Transaction;
}

export interface HlpLiquidityBuildOptions extends DuskBuildOptions {
  accounts: DuskAccounts;
  payer: AddressLike;
  owner: AddressLike;
  market: AddressLike;
  targetHlpMint: AddressLike;
  baseMint: AddressLike;
  quoteMint: AddressLike;
}

export interface HlpLiquidityBuild {
  baseYieldAccount: AccountMeta["pubkey"];
  quoteYieldAccount: AccountMeta["pubkey"];
  setupInstructions: TransactionInstruction[];
  actionInstruction: TransactionInstruction;
  transaction: Transaction;
}

export interface GovernanceMarketAccountOverrides {
  ylpMint?: AddressLike;
  baseHlpYlpVault?: AddressLike;
  quoteHlpYlpVault?: AddressLike;
}

export interface GovernanceHolderAccountOverrides extends GovernanceMarketAccountOverrides {
  holderYlpAccount?: AddressLike;
  baseYieldAccount?: AddressLike;
  quoteYieldAccount?: AddressLike;
}

export interface CreateParameterProposalParams extends GovernanceHolderAccountOverrides {
  proposer: AddressLike;
  market: AddressLike;
  nonce: GovernanceIntegerLike;
  update: ParameterUpdate;
  metadata: ProposalMetadataV1;
  initialSupport: GovernanceIntegerLike;
}

export interface SupportParameterProposalParams extends GovernanceHolderAccountOverrides {
  supporter: AddressLike;
  market: AddressLike;
  proposal: AddressLike;
  amount: GovernanceIntegerLike;
}

export interface QueueParameterProposalParams extends GovernanceMarketAccountOverrides {
  market: AddressLike;
  proposal: AddressLike;
}

export interface ExecuteParameterProposalParams {
  market: AddressLike;
  proposal: AddressLike;
}

export interface WithdrawParameterSupportParams {
  supporter: AddressLike;
  market: AddressLike;
  proposal: AddressLike;
  ylpMint?: AddressLike;
  supporterYlpAccount?: AddressLike;
  baseYieldAccount?: AddressLike;
  quoteYieldAccount?: AddressLike;
}

export interface ParameterGovernanceBuild {
  proposal: PublicKey;
  proposalSupport?: PublicKey;
  instruction: TransactionInstruction;
  transaction: Transaction;
}

interface GovernanceMarketState {
  ylpMint: PublicKey;
  baseSide: { assetMint: PublicKey };
  quoteSide: { assetMint: PublicKey };
  baseHlpVault: { ylpVault: PublicKey };
  quoteHlpVault: { ylpVault: PublicKey };
}

type AnchorMethodBuilder = {
  accounts(accounts: DuskAccounts): AnchorMethodBuilder;
  remainingAccounts(accounts: AccountMeta[]): AnchorMethodBuilder;
  instruction(): Promise<TransactionInstruction>;
  transaction(): Promise<Transaction>;
  rpc(): Promise<string>;
};

type AnchorMethods = Record<string, (...args: unknown[]) => AnchorMethodBuilder>;

export class DuskWrite {
  constructor(readonly program: Program<Dusk>) {}

  method(name: DuskInstructionName, args?: DuskInstructionArgs): AnchorMethodBuilder {
    const method = (this.program.methods as unknown as AnchorMethods)[name];
    if (!method) {
      throw new Error(`Unknown Dusk instruction: ${name}`);
    }
    return method(...normalizeArgs(args));
  }

  builder(
    name: DuskInstructionName,
    args?: DuskInstructionArgs,
    options: DuskBuildOptions = {}
  ): AnchorMethodBuilder {
    let builder = this.method(name, args);
    const instructionAccounts = this.program.idl.instructions?.find(
      (instruction) => instruction.name === name
    )?.accounts;
    const usesEventCpi =
      instructionAccounts?.some((account) => account.name === "eventAuthority") === true &&
      instructionAccounts.some((account) => account.name === "program") === true;
    if (options.accounts || usesEventCpi) {
      const accounts = usesEventCpi
        ? {
            ...options.accounts,
            eventAuthority: deriveEventAuthorityAddress(this.program.programId)[0],
            program: this.program.programId,
          }
        : options.accounts!;
      builder = builder.accounts(normalizeAccountKeys(accounts));
    }
    if (options.remainingAccounts?.length) {
      builder = builder.remainingAccounts(options.remainingAccounts);
    }
    return builder;
  }

  instruction(
    name: DuskInstructionName,
    args?: DuskInstructionArgs,
    options?: DuskBuildOptions
  ): Promise<TransactionInstruction> {
    return this.builder(name, args, options).instruction();
  }

  transaction(
    name: DuskInstructionName,
    args?: DuskInstructionArgs,
    options?: DuskBuildOptions
  ): Promise<Transaction> {
    return this.builder(name, args, options).transaction();
  }

  rpc(name: DuskInstructionName, args?: DuskInstructionArgs, options?: DuskBuildOptions) {
    return this.builder(name, args, options).rpc();
  }

  /**
   * The hLP accounts an instruction must carry when hedged liquidity is live.
   *
   * Swap and openLeverage both require them, and their order is
   * consensus-visible, so both take them from here rather than each assembling
   * its own list.
   */
  private async hlpRemainingAccounts(
    marketAddress: AddressLike
  ): Promise<AccountMeta[]> {
    const market = address(marketAddress);
    const state = (await this.program.account.market.fetch(market)) as unknown as {
      ylpMint: AccountMeta["pubkey"];
      baseSide: { interestVault: AccountMeta["pubkey"] };
      quoteSide: { interestVault: AccountMeta["pubkey"] };
      baseHlpVault: {
        hlpSupply: { toString(): string };
        residualExposure: { toString(): string };
        ylpVault: AccountMeta["pubkey"];
      };
      quoteHlpVault: {
        hlpSupply: { toString(): string };
        residualExposure: { toString(): string };
        ylpVault: AccountMeta["pubkey"];
      };
    };
    const hlpActive =
      BigInt(state.baseHlpVault.hlpSupply.toString()) !== 0n ||
      BigInt(state.quoteHlpVault.hlpSupply.toString()) !== 0n ||
      BigInt(state.baseHlpVault.residualExposure.toString()) !== 0n ||
      BigInt(state.quoteHlpVault.residualExposure.toString()) !== 0n;
    if (!hlpActive) return [];
    return [
      { pubkey: state.ylpMint, isSigner: false, isWritable: true },
      { pubkey: state.baseHlpVault.ylpVault, isSigner: false, isWritable: true },
      { pubkey: state.quoteHlpVault.ylpVault, isSigner: false, isWritable: true },
      { pubkey: state.baseSide.interestVault, isSigner: false, isWritable: true },
      { pubkey: state.quoteSide.interestVault, isSigner: false, isWritable: true },
    ];
  }

  async swapBuilder(
    args: Record<string, unknown>,
    options: SwapBuildOptions
  ): Promise<AnchorMethodBuilder> {
    const market = address(options.market);
    const state = (await this.program.account.market.fetch(market)) as unknown as {
      ylpMint: AccountMeta["pubkey"];
      baseSide: { interestVault: AccountMeta["pubkey"] };
      quoteSide: { interestVault: AccountMeta["pubkey"] };
      baseHlpVault: {
        hlpSupply: { toString(): string };
        residualExposure: { toString(): string };
        ylpVault: AccountMeta["pubkey"];
      };
      quoteHlpVault: {
        hlpSupply: { toString(): string };
        residualExposure: { toString(): string };
        ylpVault: AccountMeta["pubkey"];
      };
    };
    const hlpActive =
      BigInt(state.baseHlpVault.hlpSupply.toString()) !== 0n ||
      BigInt(state.quoteHlpVault.hlpSupply.toString()) !== 0n ||
      BigInt(state.baseHlpVault.residualExposure.toString()) !== 0n ||
      BigInt(state.quoteHlpVault.residualExposure.toString()) !== 0n;
    const prefix: AccountMeta[] = hlpActive
      ? [
          { pubkey: state.ylpMint, isSigner: false, isWritable: true },
          { pubkey: state.baseHlpVault.ylpVault, isSigner: false, isWritable: true },
          { pubkey: state.quoteHlpVault.ylpVault, isSigner: false, isWritable: true },
          { pubkey: state.baseSide.interestVault, isSigner: false, isWritable: true },
          { pubkey: state.quoteSide.interestVault, isSigner: false, isWritable: true },
        ]
      : [];

    return this.builder("swap", args, {
      accounts: {
        ...options.accounts,
        instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY,
      },
      // Prefix order is consensus-visible. Hook accounts must remain a tail,
      // including any deliberate duplicate pubkeys required by a hook meta list.
      remainingAccounts: [...prefix, ...(options.remainingAccounts ?? [])],
    });
  }

  async swapInstruction(args: Record<string, unknown>, options: SwapBuildOptions) {
    return (await this.swapBuilder(args, options)).instruction();
  }

  async swapTransaction(args: Record<string, unknown>, options: SwapBuildOptions) {
    return (await this.swapBuilder(args, options)).transaction();
  }

  async swapRpc(args: Record<string, unknown>, options: SwapBuildOptions) {
    return (await this.swapBuilder(args, options)).rpc();
  }

  /**
   * Swap, with accounts resolved from the market and the two mints.
   *
   * `swapInstruction` takes a prepared account map, which makes callers
   * responsible for reconstructing the program's account set. This resolves it
   * instead: reserve vaults derive from the market and mint, trader accounts
   * default to the associated token account for the correct token program, and
   * the hLP remaining-account prefix is still assembled by `swapBuilder`,
   * whose ordering is consensus-visible.
   */
  async buildSwapInstruction(
    params: SwapParams
  ): Promise<TransactionInstruction> {
    return (await this.buildSwapBuilder(params)).instruction();
  }

  async buildSwapTransaction(params: SwapParams): Promise<Transaction> {
    return (await this.buildSwapBuilder(params)).transaction();
  }

  /**
   * Borrow against a position, with accounts resolved from the market and the
   * two mints.
   *
   * The referral accounts are optional on chain and are omitted here: a borrow
   * with no referrer should not have to name accounts it does not use. Use
   * `referredBorrow` when a referrer is present.
   */
  async buildBorrowInstruction(
    params: BorrowParams
  ): Promise<TransactionInstruction> {
    const market = address(params.market);
    const owner = address(params.owner);
    const debtAssetMint = address(params.debtAssetMint);
    const collateralAssetMint = address(params.collateralAssetMint);

    if (debtAssetMint.equals(collateralAssetMint)) {
      throw new Error("Borrow debt and collateral mints must differ");
    }

    const debtTokenProgram = await tokenProgramForMint(
      this.program.provider.connection,
      debtAssetMint
    );

    return this.instruction(
      "borrow" as DuskInstructionName,
      {
        borrowAmount: governanceIntegerBN(params.borrowAmount, "borrowAmount"),
        minDebtAmountOut: governanceIntegerBN(
          params.minDebtAmountOut,
          "minDebtAmountOut"
        ),
        minLiquidationCfBps: Number(params.minLiquidationCfBps ?? 0),
        referrer: null,
      },
      {
        accounts: {
          market,
          futarchyAuthority: deriveFutarchyAuthorityAddress()[0],
          owner,
          debtAssetMint,
          collateralAssetMint,
          reserveVault: address(
            params.reserveVault ??
              deriveMarketReserveVaultAddress(market, debtAssetMint)[0]
          ),
          ownerDebtAccount: address(
            params.ownerDebtAccount ??
              getAssociatedTokenAddressSync(
                debtAssetMint,
                owner,
                true,
                debtTokenProgram
              )
          ),
          borrowPosition: address(
            params.borrowPosition ??
              deriveBorrowPositionAddress(market, address(params.positionId))[0]
          ),
          referralPartner: null,
          referralAccrual: null,
          tokenProgram: TOKEN_PROGRAM_ID,
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
        remainingAccounts: params.remainingAccounts,
      }
    );
  }

  async buildBorrowTransaction(params: BorrowParams): Promise<Transaction> {
    return new Transaction().add(await this.buildBorrowInstruction(params));
  }

  /**
   * Open a leverage position, with accounts resolved from the market, the
   * position id and the two mints.
   *
   * Referral accounts are optional on chain and omitted here; use
   * `referredOpenLeverage` when a referrer is present.
   */
  async buildOpenLeverageInstruction(
    params: OpenLeverageParams
  ): Promise<TransactionInstruction> {
    const market = address(params.market);
    const owner = address(params.owner);
    const debtMint = address(params.debtMint);
    const collateralMint = address(params.collateralMint);

    if (debtMint.equals(collateralMint)) {
      throw new Error("Leverage debt and collateral mints must differ");
    }

    const positionId = address(params.positionId);
    const debtTokenProgram = await tokenProgramForMint(
      this.program.provider.connection,
      debtMint
    );

    return this.instruction(
      "openLeverage" as DuskInstructionName,
      {
        positionId,
        debtAsset: params.debtAsset === "quote" ? 1 : 0,
        marginAmount: governanceIntegerBN(params.marginAmount, "marginAmount"),
        multiplierBps: governanceIntegerBN(
          params.multiplierBps,
          "multiplierBps"
        ),
        minCollateralOut: governanceIntegerBN(
          params.minCollateralOut,
          "minCollateralOut"
        ),
        referrer: null,
        positionOwner: null,
        limitPriceNad: governanceIntegerBN(
          params.limitPriceNad ?? 0,
          "limitPriceNad"
        ),
      },
      {
        accounts: {
          market,
          futarchyAuthority: deriveFutarchyAuthorityAddress()[0],
          owner,
          payer: address(params.payer ?? owner),
          leveragePosition: address(
            params.leveragePosition ??
              deriveLeveragePositionAddress(market, positionId)[0]
          ),
          debtMint,
          collateralMint,
          debtReserveVault: address(
            params.debtReserveVault ??
              deriveMarketReserveVaultAddress(market, debtMint)[0]
          ),
          collateralReserveVault: address(
            params.collateralReserveVault ??
              deriveMarketReserveVaultAddress(market, collateralMint)[0]
          ),
          leverageCollateralVault: address(
            params.leverageCollateralVault ??
              deriveLeverageCollateralVaultAddress(market, collateralMint)[0]
          ),
          ownerDebtAccount: address(
            params.ownerDebtAccount ??
              getAssociatedTokenAddressSync(
                debtMint,
                owner,
                true,
                debtTokenProgram
              )
          ),
          referralPartner: null,
          referralAccrual: null,
          instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY,
          tokenProgram: TOKEN_PROGRAM_ID,
          token2022Program: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
        },
        remainingAccounts: [
          ...(await this.hlpRemainingAccounts(market)),
          ...(params.remainingAccounts ?? []),
        ],
      }
    );
  }

  async buildOpenLeverageTransaction(
    params: OpenLeverageParams
  ): Promise<Transaction> {
    return new Transaction().add(
      await this.buildOpenLeverageInstruction(params)
    );
  }

  /**
   * Delegate a leverage position to a program that may act on it, which is how
   * conditional orders (take-profit, stop-loss) are authorised.
   */
  async buildCreateLeverageDelegationInstruction(
    params: CreateLeverageDelegationParams
  ): Promise<TransactionInstruction> {
    const market = address(params.market);
    const leveragePosition = address(
      params.leveragePosition ??
        deriveLeveragePositionAddress(market, address(params.positionId))[0]
    );

    return this.instruction(
      "createLeverageDelegation" as DuskInstructionName,
      {
        debtAsset: params.debtAsset === "quote" ? 1 : 0,
        delegatedProgram: address(params.delegatedProgram),
        approvedActions: Number(params.approvedActions),
      },
      {
        accounts: {
          market,
          leveragePosition,
          leverageDelegation: address(
            params.leverageDelegation ??
              deriveLeverageDelegationAddress(leveragePosition)[0]
          ),
          owner: address(params.owner),
          systemProgram: SystemProgram.programId,
        },
        remainingAccounts: params.remainingAccounts,
      }
    );
  }

  async buildCreateLeverageDelegationTransaction(
    params: CreateLeverageDelegationParams
  ): Promise<Transaction> {
    return new Transaction().add(
      await this.buildCreateLeverageDelegationInstruction(params)
    );
  }

  private async buildSwapBuilder(params: SwapParams) {
    const market = address(params.market);
    const trader = address(params.trader);
    const assetInMint = address(params.assetInMint);
    const assetOutMint = address(params.assetOutMint);

    if (assetInMint.equals(assetOutMint)) {
      throw new Error("Swap input and output mints must differ");
    }

    const connection = this.program.provider.connection;
    const [assetInProgram, assetOutProgram] = await Promise.all([
      tokenProgramForMint(connection, assetInMint),
      tokenProgramForMint(connection, assetOutMint),
    ]);

    return this.swapBuilder(
      // A single positional SwapArgs struct: the generic builder spreads a
      // non-array into one argument, so wrapping it in an outer object would
      // serialize a struct of zeros and the program would reject the amount.
      {
        exactAssetIn: governanceIntegerBN(params.exactAssetIn, "exactAssetIn"),
        minAssetOut: governanceIntegerBN(params.minAssetOut, "minAssetOut"),
      },
      {
        market,
        accounts: {
          market,
          futarchyAuthority: deriveFutarchyAuthorityAddress()[0],
          trader,
          assetInMint,
          assetOutMint,
          reserveInVault: address(
            params.reserveInVault ??
              deriveMarketReserveVaultAddress(market, assetInMint)[0]
          ),
          reserveOutVault: address(
            params.reserveOutVault ??
              deriveMarketReserveVaultAddress(market, assetOutMint)[0]
          ),
          traderAssetInAccount: address(
            params.traderAssetInAccount ??
              getAssociatedTokenAddressSync(
                assetInMint,
                trader,
                true,
                assetInProgram
              )
          ),
          traderAssetOutAccount: address(
            params.traderAssetOutAccount ??
              getAssociatedTokenAddressSync(
                assetOutMint,
                trader,
                true,
                assetOutProgram
              )
          ),
          tokenProgram: TOKEN_PROGRAM_ID,
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
        remainingAccounts: params.remainingAccounts,
      }
    );
  }

  async initializeYieldAccountsInstruction(params: {
    payer: AddressLike;
    owner: AddressLike;
    market: AddressLike;
    lpMint: AddressLike;
    baseMint: AddressLike;
    quoteMint: AddressLike;
    tokenKind: YieldTokenKind;
  }): Promise<TransactionInstruction> {
    const payer = address(params.payer);
    const owner = address(params.owner);
    const market = address(params.market);
    const lpMint = address(params.lpMint);
    const baseMint = address(params.baseMint);
    const quoteMint = address(params.quoteMint);
    const tokenKind = params.tokenKind === "ylp" || params.tokenKind === 0 ? { ylp: {} } : { hlp: {} };
    return this.instruction("initializeYieldAccounts" as DuskInstructionName, { owner, tokenKind }, {
      accounts: {
        payer,
        market,
        lpMint,
        baseMint,
        quoteMint,
        baseYieldAccount: deriveYieldAccountAddress(
          market,
          owner,
          lpMint,
          baseMint,
          params.tokenKind,
          this.program.programId
        )[0],
        quoteYieldAccount: deriveYieldAccountAddress(
          market,
          owner,
          lpMint,
          quoteMint,
          params.tokenKind,
          this.program.programId
        )[0],
        systemProgram: SystemProgram.programId,
      },
    });
  }

  async initializeYieldAccountsTransaction(
    params: Parameters<DuskWrite["initializeYieldAccountsInstruction"]>[0]
  ): Promise<Transaction> {
    return new Transaction().add(await this.initializeYieldAccountsInstruction(params));
  }

  async initializeLpTransferHookInstruction(params: {
    payer: AddressLike;
    market: AddressLike;
    lpMint: AddressLike;
  }): Promise<TransactionInstruction> {
    const lpMint = address(params.lpMint);
    return this.instruction("initializeLpTransferHook" as DuskInstructionName, undefined, {
      accounts: {
        payer: address(params.payer),
        market: address(params.market),
        lpMint,
        validationAccount: deriveYieldTransferHookValidationAddress(lpMint, this.program.programId)[0],
        systemProgram: SystemProgram.programId,
      },
    });
  }

  async initializeLpTransferHookTransaction(
    params: Parameters<DuskWrite["initializeLpTransferHookInstruction"]>[0]
  ): Promise<Transaction> {
    return new Transaction().add(await this.initializeLpTransferHookInstruction(params));
  }

  /** Burn-lock initial direct-yLP support and create one immutable typed proposal. */
  async createParameterProposal(
    params: CreateParameterProposalParams
  ): Promise<ParameterGovernanceBuild> {
    assertProposalMetadata(params.metadata);
    const proposer = address(params.proposer);
    const market = address(params.market);
    const state = await this.governanceMarketState(market);
    const ylpMint = address(params.ylpMint ?? state.ylpMint);
    const [proposal] = deriveParameterProposalAddress(
      market,
      proposer,
      params.nonce,
      this.program.programId
    );
    const [proposalSupport] = deriveProposalSupportAddress(
      proposal,
      proposer,
      this.program.programId
    );
    const proposerYlpAccount = address(
      params.holderYlpAccount ??
        getAssociatedTokenAddressSync(ylpMint, proposer, true, TOKEN_2022_PROGRAM_ID)
    );
    const instruction = await this.instruction(
      "createParameterProposal" as DuskInstructionName,
      {
        nonce: governanceIntegerBN(params.nonce, "nonce"),
        update: anchorParameterUpdate(params.update),
        metadata: cloneProposalMetadata(params.metadata),
        initialSupport: governanceIntegerBN(params.initialSupport, "initialSupport"),
      },
      {
        accounts: {
          proposer,
          market,
          proposal,
          proposalSupport,
          ylpMint,
          proposerYlpAccount,
          baseYieldAccount: address(
            params.baseYieldAccount ??
              deriveYieldAccountAddress(
                market,
                proposer,
                ylpMint,
                state.baseSide.assetMint,
                "ylp",
                this.program.programId
              )[0]
          ),
          quoteYieldAccount: address(
            params.quoteYieldAccount ??
              deriveYieldAccountAddress(
                market,
                proposer,
                ylpMint,
                state.quoteSide.assetMint,
                "ylp",
                this.program.programId
              )[0]
          ),
          baseHlpYlpVault: address(params.baseHlpYlpVault ?? state.baseHlpVault.ylpVault),
          quoteHlpYlpVault: address(params.quoteHlpYlpVault ?? state.quoteHlpVault.ylpVault),
          token2022Program: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
        },
      }
    );
    return {
      proposal,
      proposalSupport,
      instruction,
      transaction: new Transaction().add(instruction),
    };
  }

  /** Add burn-locked direct-yLP support; crossing strict majority queues atomically. */
  async supportParameterProposal(
    params: SupportParameterProposalParams
  ): Promise<ParameterGovernanceBuild> {
    const supporter = address(params.supporter);
    const market = address(params.market);
    const proposal = address(params.proposal);
    const state = await this.governanceMarketState(market);
    const ylpMint = address(params.ylpMint ?? state.ylpMint);
    const [proposalSupport] = deriveProposalSupportAddress(
      proposal,
      supporter,
      this.program.programId
    );
    const supporterYlpAccount = address(
      params.holderYlpAccount ??
        getAssociatedTokenAddressSync(ylpMint, supporter, true, TOKEN_2022_PROGRAM_ID)
    );
    const instruction = await this.instruction(
      "supportParameterProposal" as DuskInstructionName,
      { amount: governanceIntegerBN(params.amount, "amount") },
      {
        accounts: {
          supporter,
          market,
          proposal,
          proposalSupport,
          ylpMint,
          supporterYlpAccount,
          baseYieldAccount: address(
            params.baseYieldAccount ??
              deriveYieldAccountAddress(
                market,
                supporter,
                ylpMint,
                state.baseSide.assetMint,
                "ylp",
                this.program.programId
              )[0]
          ),
          quoteYieldAccount: address(
            params.quoteYieldAccount ??
              deriveYieldAccountAddress(
                market,
                supporter,
                ylpMint,
                state.quoteSide.assetMint,
                "ylp",
                this.program.programId
              )[0]
          ),
          baseHlpYlpVault: address(params.baseHlpYlpVault ?? state.baseHlpVault.ylpVault),
          quoteHlpYlpVault: address(params.quoteHlpYlpVault ?? state.quoteHlpVault.ylpVault),
          token2022Program: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
        },
      }
    );
    return {
      proposal,
      proposalSupport,
      instruction,
      transaction: new Transaction().add(instruction),
    };
  }

  /** Permissionlessly queue after the denominator falls below already-locked support. */
  async queueParameterProposal(
    params: QueueParameterProposalParams
  ): Promise<ParameterGovernanceBuild> {
    const market = address(params.market);
    const proposal = address(params.proposal);
    const state = await this.governanceMarketState(market);
    const instruction = await this.instruction(
      "queueParameterProposal" as DuskInstructionName,
      undefined,
      {
        accounts: {
          market,
          proposal,
          ylpMint: address(params.ylpMint ?? state.ylpMint),
          baseHlpYlpVault: address(params.baseHlpYlpVault ?? state.baseHlpVault.ylpVault),
          quoteHlpYlpVault: address(params.quoteHlpYlpVault ?? state.quoteHlpVault.ylpVault),
        },
      }
    );
    return { proposal, instruction, transaction: new Transaction().add(instruction) };
  }

  /** Permissionlessly execute inside the 7-day window; on-chain utilization checks remain authoritative. */
  async executeParameterProposal(
    params: ExecuteParameterProposalParams
  ): Promise<ParameterGovernanceBuild> {
    const proposal = address(params.proposal);
    const instruction = await this.instruction(
      "executeParameterProposal" as DuskInstructionName,
      undefined,
      { accounts: { market: params.market, proposal } }
    );
    return { proposal, instruction, transaction: new Transaction().add(instruction) };
  }

  /** Mint back exactly one support position's burned yLP and merge its virtual yield. */
  async withdrawParameterSupport(
    params: WithdrawParameterSupportParams
  ): Promise<ParameterGovernanceBuild> {
    const supporter = address(params.supporter);
    const market = address(params.market);
    const proposal = address(params.proposal);
    const state = await this.governanceMarketState(market);
    const ylpMint = address(params.ylpMint ?? state.ylpMint);
    const [proposalSupport] = deriveProposalSupportAddress(
      proposal,
      supporter,
      this.program.programId
    );
    const supporterYlpAccount = address(
      params.supporterYlpAccount ??
        getAssociatedTokenAddressSync(ylpMint, supporter, true, TOKEN_2022_PROGRAM_ID)
    );
    const instruction = await this.instruction(
      "withdrawParameterSupport" as DuskInstructionName,
      undefined,
      {
        accounts: {
          supporter,
          market,
          proposal,
          proposalSupport,
          ylpMint,
          supporterYlpAccount,
          baseYieldAccount: address(
            params.baseYieldAccount ??
              deriveYieldAccountAddress(
                market,
                supporter,
                ylpMint,
                state.baseSide.assetMint,
                "ylp",
                this.program.programId
              )[0]
          ),
          quoteYieldAccount: address(
            params.quoteYieldAccount ??
              deriveYieldAccountAddress(
                market,
                supporter,
                ylpMint,
                state.quoteSide.assetMint,
                "ylp",
                this.program.programId
              )[0]
          ),
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
      }
    );
    return {
      proposal,
      proposalSupport,
      instruction,
      transaction: new Transaction().add(instruction),
    };
  }

  private async governanceMarketState(market: PublicKey): Promise<GovernanceMarketState> {
    return (await this.program.account.market.fetch(market)) as unknown as GovernanceMarketState;
  }

  async hlpLiquidityAction(
    name: "depositSingleSided" | "withdrawSingleSided",
    args: Record<string, unknown>,
    options: HlpLiquidityBuildOptions
  ): Promise<HlpLiquidityBuild> {
    const payer = address(options.payer);
    const owner = address(options.owner);
    const market = address(options.market);
    const lpMint = address(options.targetHlpMint);
    const baseMint = address(options.baseMint);
    const quoteMint = address(options.quoteMint);
    const baseYieldAccount = deriveYieldAccountAddress(
      market,
      owner,
      lpMint,
      baseMint,
      "hlp",
      this.program.programId
    )[0];
    const quoteYieldAccount = deriveYieldAccountAddress(
      market,
      owner,
      lpMint,
      quoteMint,
      "hlp",
      this.program.programId
    )[0];
    const [baseYieldInfo, quoteYieldInfo] = await Promise.all([
      this.program.provider.connection.getAccountInfo(baseYieldAccount),
      this.program.provider.connection.getAccountInfo(quoteYieldAccount),
    ]);
    const yieldAccountDefinition = this.program.idl.accounts?.find(
      (account) => account.name === "yieldAccount"
    );
    if (!yieldAccountDefinition) {
      throw new Error("Dusk IDL is missing YieldAccount");
    }
    const yieldAccountSize = this.program.coder.accounts.size("yieldAccount");
    const yieldAccountDiscriminator = Buffer.from(yieldAccountDefinition.discriminator);
    const yieldAccountsReady = [baseYieldInfo, quoteYieldInfo].every(
      (info) =>
        info !== null &&
        info.owner.equals(this.program.programId) &&
        info.data.length === yieldAccountSize &&
        info.data.subarray(0, yieldAccountDiscriminator.length).equals(yieldAccountDiscriminator)
    );
    const setupInstructions =
      yieldAccountsReady
        ? []
        : [
            await this.initializeYieldAccountsInstruction({
              payer,
              owner,
              market,
              lpMint,
              baseMint,
              quoteMint,
              tokenKind: "hlp",
            }),
          ];
    const actionInstruction = await this.instruction(name, args, {
      accounts: {
        ...options.accounts,
        market,
        owner,
        targetHlpMint: lpMint,
        baseMint,
        quoteMint,
        baseYieldAccount,
        quoteYieldAccount,
      },
      remainingAccounts: options.remainingAccounts,
    });
    return {
      baseYieldAccount,
      quoteYieldAccount,
      setupInstructions,
      actionInstruction,
      transaction: new Transaction().add(...setupInstructions, actionInstruction),
    };
  }

  depositSingleSided(args: Record<string, unknown>, options: HlpLiquidityBuildOptions) {
    return this.hlpLiquidityAction("depositSingleSided", args, options);
  }

  withdrawSingleSided(args: Record<string, unknown>, options: HlpLiquidityBuildOptions) {
    return this.hlpLiquidityAction("withdrawSingleSided", args, options);
  }

  async configureReferralPartnerInstruction(params: {
    authoritySigner: AddressLike;
    referrer: AddressLike;
    interestShareBps: number;
    active: boolean;
    futarchyAuthority?: AddressLike;
  }): Promise<TransactionInstruction> {
    assertReferralInterestShareBps(params.interestShareBps);
    const referrer = address(params.referrer);
    return this.instruction(
      "configureReferralPartner",
      {
        referrer,
        interestShareBps: params.interestShareBps,
        active: params.active,
      },
      {
        accounts: {
          authoritySigner: address(params.authoritySigner),
          futarchyAuthority:
            params.futarchyAuthority ?? deriveFutarchyAuthorityAddress()[0],
          referralPartner: deriveReferralPartnerAddress(referrer)[0],
          systemProgram: SystemProgram.programId,
        },
      }
    );
  }

  async configureReferralPartnerTransaction(
    params: Parameters<DuskWrite["configureReferralPartnerInstruction"]>[0]
  ): Promise<Transaction> {
    return new Transaction().add(await this.configureReferralPartnerInstruction(params));
  }

  async initializeReferralAccrualInstruction(params: {
    payer: AddressLike;
    referrer: AddressLike;
    market: AddressLike;
    assetMint: AddressLike;
  }): Promise<TransactionInstruction> {
    const referral = referralAccrualAddresses(params);
    return this.instruction("initializeReferralAccrual", undefined, {
      accounts: {
        payer: address(params.payer),
        referralPartner: referral.referralPartner,
        market: address(params.market),
        assetMint: address(params.assetMint),
        referralAccrual: referral.referralAccrual,
        systemProgram: SystemProgram.programId,
      },
    });
  }

  async initializeReferralAccrualTransaction(
    params: Parameters<DuskWrite["initializeReferralAccrualInstruction"]>[0]
  ): Promise<Transaction> {
    return new Transaction().add(
      await this.initializeReferralAccrualInstruction(params)
    );
  }

  async referredAction(
    name: ReferredActionName,
    args: Record<string, unknown>,
    options: ReferredActionOptions
  ): Promise<ReferredActionBuild> {
    const referral = referralAccrualAddresses({
      referrer: options.referrer,
      market: options.market,
      assetMint: options.debtMint,
    });
    const setupInstruction = await this.initializeReferralAccrualInstruction({
      payer: options.payer,
      referrer: options.referrer,
      market: options.market,
      assetMint: options.debtMint,
    });
    const hookAccounts = options.transferHookTransfers?.length
      ? await resolveTransferHookAccountMetas(
          this.program.provider.connection,
          options.transferHookTransfers
        )
      : [];
    const actionInstruction = await this.instruction(
      name,
      {
        ...args,
        referrer: address(options.referrer),
      },
      {
        accounts: {
          ...options.accounts,
          referralPartner: referral.referralPartner,
          referralAccrual: referral.referralAccrual,
          ...(name === "openLeverage"
            ? { instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY }
            : {}),
        },
        remainingAccounts: mergeAccountMetas(options.remainingAccounts ?? [], hookAccounts),
      }
    );
    return {
      referralPartner: referral.referralPartner,
      referralAccrual: referral.referralAccrual,
      setupInstruction,
      actionInstruction,
      transaction: new Transaction().add(setupInstruction, actionInstruction),
    };
  }

  referredBorrow(args: Record<string, unknown>, options: ReferredActionOptions) {
    return this.referredAction("borrow", args, options);
  }

  referredOpenLeverage(args: Record<string, unknown>, options: ReferredActionOptions) {
    return this.referredAction("openLeverage", args, options);
  }

  async setReferralRecipientInstruction(params: {
    authority: AddressLike;
    recipient: AddressLike;
  }): Promise<TransactionInstruction> {
    const authority = address(params.authority);
    return this.instruction(
      "setReferralRecipient",
      { recipient: address(params.recipient) },
      {
        accounts: {
          authority,
          referralPartner: deriveReferralPartnerAddress(authority)[0],
        },
      }
    );
  }

  async setReferralRecipientTransaction(params: {
    authority: AddressLike;
    recipient: AddressLike;
  }): Promise<Transaction> {
    return new Transaction().add(await this.setReferralRecipientInstruction(params));
  }

  async claimReferralInterestInstruction(params: {
    authority: AddressLike;
    market: AddressLike;
    mint: AddressLike;
    interestVault?: AddressLike;
    recipientTokenAccount: AddressLike;
    remainingAccounts?: AccountMeta[];
  }): Promise<TransactionInstruction> {
    const authority = address(params.authority);
    const market = address(params.market);
    const mintKey = address(params.mint);
    const referral = referralAccrualAddresses({
      referrer: authority,
      market,
      assetMint: mintKey,
    });
    const interestVault = address(
      params.interestVault ?? deriveMarketInterestVaultAddress(market, mintKey)[0]
    );
    const recipientTokenAccount = address(params.recipientTokenAccount);
    const tokenProgram = await tokenProgramForMint(
      this.program.provider.connection,
      mintKey
    );
    const [accrual, mint] = await Promise.all([
      this.program.account.referralAccrual.fetch(referral.referralAccrual),
      getMint(
        this.program.provider.connection,
        mintKey,
        undefined,
        tokenProgram
      ),
    ]);
    const hookAccounts = await resolveTransferHookAccountMetas(
      this.program.provider.connection,
      [
        {
          source: interestVault,
          mint: mintKey,
          destination: recipientTokenAccount,
          authority: market,
          amount: accrual.amount,
          decimals: mint.decimals,
          tokenProgram,
        },
      ]
    );
    return this.instruction("claimReferralInterest", undefined, {
      accounts: {
        market,
        authority,
        referralPartner: referral.referralPartner,
        assetMint: mintKey,
        referralAccrual: referral.referralAccrual,
        interestVault,
        recipientTokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
      },
      remainingAccounts: mergeAccountMetas(params.remainingAccounts ?? [], hookAccounts),
    });
  }

  async claimReferralInterestTransaction(
    params: Parameters<DuskWrite["claimReferralInterestInstruction"]>[0]
  ): Promise<Transaction> {
    return new Transaction().add(
      await this.claimReferralInterestInstruction(params)
    );
  }

  /**
   * Deposit collateral into a borrow position, creating it when `positionId` is
   * new. Callers pass amounts in raw base units.
   */
  async depositCollateralInstruction(
    params: DepositCollateralParams
  ): Promise<TransactionInstruction> {
    const market = address(params.market);
    const owner = address(params.owner);
    const assetMint = address(params.assetMint);
    const positionId = address(params.positionId);
    const tokenProgram = await tokenProgramForMint(
      this.program.provider.connection,
      assetMint
    );
    return this.instruction(
      "depositCollateral" as DuskInstructionName,
      { positionId, depositAmount: governanceIntegerBN(params.depositAmount, "depositAmount") },
      {
        accounts: {
          market,
          owner,
          assetMint,
          collateralVault: address(
            params.collateralVault ??
              deriveMarketCollateralVaultAddress(market, assetMint)[0]
          ),
          ownerAssetAccount: address(params.ownerAssetAccount),
          borrowPosition: address(
            params.borrowPosition ??
              deriveBorrowPositionAddress(market, positionId)[0]
          ),
          tokenProgram,
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
        remainingAccounts: params.remainingAccounts,
      }
    );
  }

  async depositCollateralTransaction(
    params: DepositCollateralParams
  ): Promise<Transaction> {
    return new Transaction().add(
      await this.depositCollateralInstruction(params)
    );
  }

  /**
   * Withdraw collateral. `minAssetAmountOut` and `minLiquidationCfBps` are the
   * caller's slippage and health floors; the program rejects the withdrawal
   * rather than silently returning less.
   */
  async withdrawCollateralInstruction(
    params: WithdrawCollateralParams
  ): Promise<TransactionInstruction> {
    const market = address(params.market);
    const owner = address(params.owner);
    const assetMint = address(params.assetMint);
    const positionId = address(params.positionId);
    const tokenProgram = await tokenProgramForMint(
      this.program.provider.connection,
      assetMint
    );
    return this.instruction(
      "withdrawCollateral" as DuskInstructionName,
      {
        withdrawAmount: governanceIntegerBN(params.withdrawAmount, "withdrawAmount"),
        minAssetAmountOut: governanceIntegerBN(params.minAssetAmountOut, "minAssetAmountOut"),
        minLiquidationCfBps: params.minLiquidationCfBps,
      },
      {
        accounts: {
          market,
          futarchyAuthority: deriveFutarchyAuthorityAddress()[0],
          owner,
          assetMint,
          collateralVault: address(
            params.collateralVault ??
              deriveMarketCollateralVaultAddress(market, assetMint)[0]
          ),
          ownerAssetAccount: address(params.ownerAssetAccount),
          borrowPosition: address(
            params.borrowPosition ??
              deriveBorrowPositionAddress(market, positionId)[0]
          ),
          tokenProgram,
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
        remainingAccounts: params.remainingAccounts,
      }
    );
  }

  async withdrawCollateralTransaction(
    params: WithdrawCollateralParams
  ): Promise<Transaction> {
    return new Transaction().add(
      await this.withdrawCollateralInstruction(params)
    );
  }

  /**
   * Repay borrowed debt. Pass the wallet balance as `repayAmount` to clear a
   * position; the program repays at most the outstanding debt.
   */
  async repayInstruction(params: RepayParams): Promise<TransactionInstruction> {
    const market = address(params.market);
    const owner = address(params.owner);
    const debtAssetMint = address(params.debtAssetMint);
    const positionId = address(params.positionId);
    const referralPartner = params.referralPartner
      ? address(params.referralPartner)
      : null;
    const tokenProgram = await tokenProgramForMint(
      this.program.provider.connection,
      debtAssetMint
    );
    return this.instruction(
      "repay" as DuskInstructionName,
      { repayAmount: governanceIntegerBN(params.repayAmount, "repayAmount") },
      {
        accounts: {
          market,
          futarchyAuthority: deriveFutarchyAuthorityAddress()[0],
          owner,
          debtAssetMint,
          reserveVault: address(
            params.reserveVault ??
              deriveMarketReserveVaultAddress(market, debtAssetMint)[0]
          ),
          interestVault: address(
            params.interestVault ??
              deriveMarketInterestVaultAddress(market, debtAssetMint)[0]
          ),
          ownerDebtAccount: address(params.ownerDebtAccount),
          borrowPosition: address(
            params.borrowPosition ??
              deriveBorrowPositionAddress(market, positionId)[0]
          ),
          referralPartner,
          referralAccrual: referralPartner
            ? deriveReferralAccrualAddress(
                referralPartner,
                market,
                debtAssetMint
              )[0]
            : null,
          tokenProgram,
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
        remainingAccounts: params.remainingAccounts,
      }
    );
  }

  async repayTransaction(params: RepayParams): Promise<Transaction> {
    return new Transaction().add(await this.repayInstruction(params));
  }

  /**
   * Add balanced yLP liquidity. `minYlpAmount` is the caller's slippage floor;
   * the program rejects the deposit rather than minting fewer shares.
   */
  async addLiquidityInstruction(
    params: AddLiquidityParams
  ): Promise<TransactionInstruction> {
    const common = await this.resolveYlpAccounts(params);
    return this.instruction(
      "addLiquidity" as DuskInstructionName,
      {
        baseDepositAmount: governanceIntegerBN(
          params.baseDepositAmount,
          "baseDepositAmount"
        ),
        quoteDepositAmount: governanceIntegerBN(
          params.quoteDepositAmount,
          "quoteDepositAmount"
        ),
        minYlpAmount: governanceIntegerBN(params.minYlpAmount, "minYlpAmount"),
      },
      {
        accounts: {
          ...common,
          futarchyAuthority: deriveFutarchyAuthorityAddress()[0],
        },
        remainingAccounts: params.remainingAccounts,
      }
    );
  }

  async addLiquidityTransaction(
    params: AddLiquidityParams
  ): Promise<Transaction> {
    return new Transaction().add(await this.addLiquidityInstruction(params));
  }

  /**
   * Burn yLP for the underlying pair. Both minimums are enforced by the
   * program, so a partially-filled withdrawal fails instead of silently
   * returning less of one side.
   */
  async removeLiquidityInstruction(
    params: RemoveLiquidityParams
  ): Promise<TransactionInstruction> {
    const common = await this.resolveYlpAccounts(params);
    return this.instruction(
      "removeLiquidity" as DuskInstructionName,
      {
        ylpAmount: governanceIntegerBN(params.ylpAmount, "ylpAmount"),
        minBaseAmountOut: governanceIntegerBN(
          params.minBaseAmountOut,
          "minBaseAmountOut"
        ),
        minQuoteAmountOut: governanceIntegerBN(
          params.minQuoteAmountOut,
          "minQuoteAmountOut"
        ),
      },
      { accounts: common, remainingAccounts: params.remainingAccounts }
    );
  }

  async removeLiquidityTransaction(
    params: RemoveLiquidityParams
  ): Promise<Transaction> {
    return new Transaction().add(await this.removeLiquidityInstruction(params));
  }

  /**
   * Accounts shared by both balanced yLP instructions. Reserve vaults and yLP
   * yield accounts derive from the market; token programs resolve per mint so
   * a Token-2022 side needs no caller branching.
   */
  /** Borrow more against an open position and receive collateral. */
  async increaseLeverageInstruction(
    params: IncreaseLeverageParams
  ): Promise<TransactionInstruction> {
    const core = await this.resolveLeverageAccounts(params);
    return this.instruction(
      "increaseLeverage" as DuskInstructionName,
      {
        debtAsset: marketAssetIndex(params.debtAsset),
        debtAmount: governanceIntegerBN(params.debtAmount, "debtAmount"),
        minCollateralOut: governanceIntegerBN(
          params.minCollateralOut,
          "minCollateralOut"
        ),
      },
      {
        accounts: {
          market: core.market,
          futarchyAuthority: core.futarchyAuthority,
          positionOwner: core.positionOwner,
          leveragePosition: core.leveragePosition,
          debtMint: core.debtMint,
          collateralMint: core.collateralMint,
          debtReserveVault: core.debtReserveVault,
          collateralReserveVault: core.collateralReserveVault,
          leverageCollateralVault: core.leverageCollateralVault,
          owner: core.owner,
          instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY,
          tokenProgram: core.tokenProgram,
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
        remainingAccounts: params.remainingAccounts,
      }
    );
  }

  async increaseLeverageTransaction(
    params: IncreaseLeverageParams
  ): Promise<Transaction> {
    return new Transaction().add(
      await this.increaseLeverageInstruction(params)
    );
  }

  /** Sell collateral to repay debt without closing the position. */
  async decreaseLeverageInstruction(
    params: DecreaseLeverageParams
  ): Promise<TransactionInstruction> {
    const core = await this.resolveLeverageAccounts(params);
    return this.instruction(
      "decreaseLeverage" as DuskInstructionName,
      {
        debtAsset: marketAssetIndex(params.debtAsset),
        collateralAmount: governanceIntegerBN(
          params.collateralAmount,
          "collateralAmount"
        ),
        minRepayOut: governanceIntegerBN(params.minRepayOut, "minRepayOut"),
      },
      {
        accounts: {
          market: core.market,
          futarchyAuthority: core.futarchyAuthority,
          positionOwner: core.positionOwner,
          leveragePosition: core.leveragePosition,
          debtMint: core.debtMint,
          collateralMint: core.collateralMint,
          debtReserveVault: core.debtReserveVault,
          collateralReserveVault: core.collateralReserveVault,
          debtInterestVault: core.debtInterestVault,
          leverageCollateralVault: core.leverageCollateralVault,
          referralPartner: core.referralPartner,
          referralAccrual: core.referralAccrual,
          owner: core.owner,
          instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY,
          tokenProgram: core.tokenProgram,
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
        remainingAccounts: params.remainingAccounts,
      }
    );
  }

  async decreaseLeverageTransaction(
    params: DecreaseLeverageParams
  ): Promise<Transaction> {
    return new Transaction().add(
      await this.decreaseLeverageInstruction(params)
    );
  }

  /** Post additional debt-asset margin to a position. */
  async addLeverageMarginInstruction(
    params: LeverageMarginParams
  ): Promise<TransactionInstruction> {
    const core = await this.resolveLeverageAccounts(params);
    return this.instruction(
      "addLeverageMargin" as DuskInstructionName,
      {
        debtAsset: marketAssetIndex(params.debtAsset),
        amount: governanceIntegerBN(params.amount, "amount"),
      },
      {
        accounts: {
          market: core.market,
          futarchyAuthority: core.futarchyAuthority,
          positionOwner: core.positionOwner,
          leveragePosition: core.leveragePosition,
          debtMint: core.debtMint,
          debtReserveVault: core.debtReserveVault,
          debtInterestVault: core.debtInterestVault,
          ownerDebtAccount: address(params.ownerDebtAccount),
          referralPartner: core.referralPartner,
          referralAccrual: core.referralAccrual,
          owner: core.owner,
          tokenProgram: core.tokenProgram,
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
        remainingAccounts: params.remainingAccounts,
      }
    );
  }

  async addLeverageMarginTransaction(
    params: LeverageMarginParams
  ): Promise<Transaction> {
    return new Transaction().add(
      await this.addLeverageMarginInstruction(params)
    );
  }

  /** Withdraw surplus margin, bounded by the position's health. */
  async removeLeverageMarginInstruction(
    params: RemoveLeverageMarginParams
  ): Promise<TransactionInstruction> {
    const core = await this.resolveLeverageAccounts(params);
    return this.instruction(
      "removeLeverageMargin" as DuskInstructionName,
      {
        debtAsset: marketAssetIndex(params.debtAsset),
        amount: governanceIntegerBN(params.amount, "amount"),
        minAmountOut: governanceIntegerBN(params.minAmountOut, "minAmountOut"),
      },
      {
        accounts: {
          market: core.market,
          futarchyAuthority: core.futarchyAuthority,
          positionOwner: core.positionOwner,
          leveragePosition: core.leveragePosition,
          debtMint: core.debtMint,
          collateralMint: core.collateralMint,
          debtReserveVault: core.debtReserveVault,
          ownerDebtAccount: address(params.ownerDebtAccount),
          owner: core.owner,
          tokenProgram: core.tokenProgram,
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
        remainingAccounts: params.remainingAccounts,
      }
    );
  }

  async removeLeverageMarginTransaction(
    params: RemoveLeverageMarginParams
  ): Promise<Transaction> {
    return new Transaction().add(
      await this.removeLeverageMarginInstruction(params)
    );
  }

  /**
   * Close a position. Owner-signed by default; pass the delegation accounts to
   * settle through `leverage_delegate` instead.
   */
  async closeLeverageInstruction(
    params: CloseLeverageParams
  ): Promise<TransactionInstruction> {
    const core = await this.resolveLeverageAccounts(params);
    return this.instruction(
      "closeLeverage" as DuskInstructionName,
      {
        debtAsset: marketAssetIndex(params.debtAsset),
        minAmountOut: governanceIntegerBN(params.minAmountOut, "minAmountOut"),
      },
      {
        accounts: {
          market: core.market,
          futarchyAuthority: core.futarchyAuthority,
          positionOwner: core.positionOwner,
          leveragePosition: core.leveragePosition,
          debtMint: core.debtMint,
          collateralMint: core.collateralMint,
          debtReserveVault: core.debtReserveVault,
          collateralReserveVault: core.collateralReserveVault,
          debtInterestVault: core.debtInterestVault,
          leverageCollateralVault: core.leverageCollateralVault,
          ownerDebtAccount: address(params.ownerDebtAccount),
          referralPartner: core.referralPartner,
          referralAccrual: core.referralAccrual,
          leverageDelegation: params.leverageDelegation
            ? address(params.leverageDelegation)
            : null,
          delegatedProgram: params.delegatedProgram
            ? address(params.delegatedProgram)
            : null,
          authority: address(params.authority ?? params.positionOwner),
          instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY,
          tokenProgram: core.tokenProgram,
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
        remainingAccounts: params.remainingAccounts,
      }
    );
  }

  async closeLeverageTransaction(
    params: CloseLeverageParams
  ): Promise<Transaction> {
    return new Transaction().add(await this.closeLeverageInstruction(params));
  }

  /**
   * Accounts shared across the leverage lifecycle. Individual builders pass
   * through only the subset their instruction declares.
   */
  private async resolveLeverageAccounts(params: LeverageAccounts) {
    const market = address(params.market);
    const positionOwner = address(params.positionOwner);
    const debtMint = address(params.debtMint);
    const collateralMint = address(params.collateralMint);
    const positionId = address(params.positionId);
    const referralPartner = params.referralPartner
      ? address(params.referralPartner)
      : null;
    const tokenProgram = await tokenProgramForMint(
      this.program.provider.connection,
      debtMint
    );
    return {
      market,
      futarchyAuthority: deriveFutarchyAuthorityAddress()[0],
      positionOwner,
      owner: address(params.owner ?? params.positionOwner),
      leveragePosition: address(
        params.leveragePosition ??
          deriveLeveragePositionAddress(market, positionId)[0]
      ),
      debtMint,
      collateralMint,
      debtReserveVault: address(
        params.debtReserveVault ??
          deriveMarketReserveVaultAddress(market, debtMint)[0]
      ),
      collateralReserveVault: address(
        params.collateralReserveVault ??
          deriveMarketReserveVaultAddress(market, collateralMint)[0]
      ),
      debtInterestVault: address(
        params.debtInterestVault ??
          deriveMarketInterestVaultAddress(market, debtMint)[0]
      ),
      leverageCollateralVault: address(
        params.leverageCollateralVault ??
          deriveLeverageCollateralVaultAddress(market, collateralMint)[0]
      ),
      referralPartner,
      referralAccrual: referralPartner
        ? deriveReferralAccrualAddress(referralPartner, market, debtMint)[0]
        : null,
      tokenProgram,
    };
  }

  private async resolveYlpAccounts(params: YlpLiquidityAccounts) {
    const market = address(params.market);
    const owner = address(params.owner);
    const baseMint = address(params.baseMint);
    const quoteMint = address(params.quoteMint);
    const ylpMint = address(params.ylpMint);
    const tokenProgram = await tokenProgramForMint(
      this.program.provider.connection,
      baseMint
    );
    return {
      market,
      owner,
      baseMint,
      quoteMint,
      ylpMint,
      baseReserveVault: address(
        params.baseReserveVault ??
          deriveMarketReserveVaultAddress(market, baseMint)[0]
      ),
      quoteReserveVault: address(
        params.quoteReserveVault ??
          deriveMarketReserveVaultAddress(market, quoteMint)[0]
      ),
      ownerBaseAccount: address(params.ownerBaseAccount),
      ownerQuoteAccount: address(params.ownerQuoteAccount),
      ownerYlpAccount: address(params.ownerYlpAccount),
      baseYieldAccount: address(
        params.baseYieldAccount ??
          deriveYieldAccountAddress(
            market,
            owner,
            ylpMint,
            baseMint,
            "ylp",
            this.program.programId
          )[0]
      ),
      quoteYieldAccount: address(
        params.quoteYieldAccount ??
          deriveYieldAccountAddress(
            market,
            owner,
            ylpMint,
            quoteMint,
            "ylp",
            this.program.programId
          )[0]
      ),
      tokenProgram,
      token2022Program: TOKEN_2022_PROGRAM_ID,
    };
  }
}

/** Raw base-unit amount. Accepts bigint to keep callers off floating point. */
export type RawAmount = bigint | number | string;

interface LendingPositionAccounts {
  market: AddressLike;
  owner: AddressLike;
  /** Position discriminator; the borrow position PDA derives from it. */
  positionId: AddressLike;
  borrowPosition?: AddressLike;
  remainingAccounts?: AccountMeta[];
}

export interface DepositCollateralParams extends LendingPositionAccounts {
  assetMint: AddressLike;
  ownerAssetAccount: AddressLike;
  depositAmount: RawAmount;
  collateralVault?: AddressLike;
}

export interface WithdrawCollateralParams extends LendingPositionAccounts {
  assetMint: AddressLike;
  ownerAssetAccount: AddressLike;
  withdrawAmount: RawAmount;
  minAssetAmountOut: RawAmount;
  /** Health floor enforced by the program, in basis points. */
  minLiquidationCfBps: number;
  collateralVault?: AddressLike;
}

export interface RepayParams extends LendingPositionAccounts {
  debtAssetMint: AddressLike;
  ownerDebtAccount: AddressLike;
  repayAmount: RawAmount;
  /** Omit when the position has no referrer. */
  referralPartner?: AddressLike | null;
  reserveVault?: AddressLike;
  interestVault?: AddressLike;
}

/** Which side of the pair carries the debt. */
export type MarketAssetSide = "base" | "quote";

function marketAssetIndex(side: MarketAssetSide): number {
  if (side !== "base" && side !== "quote") {
    throw new Error(`debtAsset must be "base" or "quote", got ${String(side)}`);
  }
  return side === "base" ? 0 : 1;
}

interface LeverageAccounts {
  market: AddressLike;
  positionOwner: AddressLike;
  /** Position discriminator; the leverage position PDA derives from it. */
  positionId: AddressLike;
  debtMint: AddressLike;
  collateralMint: AddressLike;
  debtAsset: MarketAssetSide;
  /** Signer when it differs from the position owner. */
  owner?: AddressLike;
  leveragePosition?: AddressLike;
  debtReserveVault?: AddressLike;
  collateralReserveVault?: AddressLike;
  debtInterestVault?: AddressLike;
  leverageCollateralVault?: AddressLike;
  referralPartner?: AddressLike | null;
  remainingAccounts?: AccountMeta[];
}

export interface IncreaseLeverageParams extends LeverageAccounts {
  debtAmount: RawAmount;
  minCollateralOut: RawAmount;
}

export interface DecreaseLeverageParams extends LeverageAccounts {
  collateralAmount: RawAmount;
  minRepayOut: RawAmount;
}

export interface LeverageMarginParams extends LeverageAccounts {
  amount: RawAmount;
  ownerDebtAccount: AddressLike;
}

export interface RemoveLeverageMarginParams extends LeverageMarginParams {
  minAmountOut: RawAmount;
}

export interface CloseLeverageParams extends LeverageAccounts {
  minAmountOut: RawAmount;
  ownerDebtAccount: AddressLike;
  /** Delegated settlement; omit all three for an owner-signed close. */
  leverageDelegation?: AddressLike | null;
  delegatedProgram?: AddressLike | null;
  authority?: AddressLike;
}

interface YlpLiquidityAccounts {
  market: AddressLike;
  owner: AddressLike;
  baseMint: AddressLike;
  quoteMint: AddressLike;
  ylpMint: AddressLike;
  ownerBaseAccount: AddressLike;
  ownerQuoteAccount: AddressLike;
  ownerYlpAccount: AddressLike;
  baseReserveVault?: AddressLike;
  quoteReserveVault?: AddressLike;
  baseYieldAccount?: AddressLike;
  quoteYieldAccount?: AddressLike;
  remainingAccounts?: AccountMeta[];
}

/** Which side of the market a leverage position borrows. */
export type LeverageDebtAsset = "base" | "quote";

/** Opening leverage, described by the market, position and mints. */
export interface OpenLeverageParams {
  market: AddressLike;
  owner: AddressLike;
  positionId: AddressLike;
  debtAsset: LeverageDebtAsset;
  debtMint: AddressLike;
  collateralMint: AddressLike;
  marginAmount: RawAmount;
  /** Leverage multiplier in basis points; 20000 is two times. */
  multiplierBps: RawAmount;
  minCollateralOut: RawAmount;
  /** Zero means no limit price. */
  limitPriceNad?: RawAmount;
  payer?: AddressLike;
  ownerDebtAccount?: AddressLike;
  leveragePosition?: AddressLike;
  debtReserveVault?: AddressLike;
  collateralReserveVault?: AddressLike;
  leverageCollateralVault?: AddressLike;
  remainingAccounts?: AccountMeta[];
}

/** Authorising a program to act on a leverage position. */
export interface CreateLeverageDelegationParams {
  market: AddressLike;
  owner: AddressLike;
  positionId: AddressLike;
  debtAsset: LeverageDebtAsset;
  delegatedProgram: AddressLike;
  /** Bit flags for the actions the delegate may perform. */
  approvedActions: number;
  leveragePosition?: AddressLike;
  leverageDelegation?: AddressLike;
  remainingAccounts?: AccountMeta[];
}

/** A borrow described by its market, position and mints. */
export interface BorrowParams {
  market: AddressLike;
  owner: AddressLike;
  /** Position discriminator; the borrow position PDA derives from it. */
  positionId: AddressLike;
  debtAssetMint: AddressLike;
  collateralAssetMint: AddressLike;
  borrowAmount: RawAmount;
  /** Slippage floor on the debt actually opened. */
  minDebtAmountOut: RawAmount;
  minLiquidationCfBps?: number;
  ownerDebtAccount?: AddressLike;
  reserveVault?: AddressLike;
  borrowPosition?: AddressLike;
  remainingAccounts?: AccountMeta[];
}

/** A swap described by its market and mints rather than by an account map. */
export interface SwapParams {
  market: AddressLike;
  trader: AddressLike;
  assetInMint: AddressLike;
  assetOutMint: AddressLike;
  exactAssetIn: RawAmount;
  /** Slippage floor; the program rejects a fill below it. */
  minAssetOut: RawAmount;
  traderAssetInAccount?: AddressLike;
  traderAssetOutAccount?: AddressLike;
  reserveInVault?: AddressLike;
  reserveOutVault?: AddressLike;
  remainingAccounts?: AccountMeta[];
}

export interface AddLiquidityParams extends YlpLiquidityAccounts {
  baseDepositAmount: RawAmount;
  quoteDepositAmount: RawAmount;
  /** Slippage floor on minted yLP shares. */
  minYlpAmount: RawAmount;
}

export interface RemoveLiquidityParams extends YlpLiquidityAccounts {
  ylpAmount: RawAmount;
  minBaseAmountOut: RawAmount;
  minQuoteAmountOut: RawAmount;
}

function normalizeArgs(args: DuskInstructionArgs): unknown[] {
  if (args === undefined) {
    return [];
  }
  return Array.isArray(args) ? args : [args];
}

function cloneProposalMetadata(metadata: ProposalMetadataV1): ProposalMetadataV1 {
  return {
    version: metadata.version,
    title: metadata.title,
    descriptionUri: metadata.descriptionUri,
    descriptionSha256: [...metadata.descriptionSha256],
    descriptionLen: metadata.descriptionLen,
  };
}

function mergeAccountMetas(...groups: readonly AccountMeta[][]): AccountMeta[] {
  const merged = new Map<string, AccountMeta>();
  for (const group of groups) {
    for (const meta of group) {
      const key = meta.pubkey.toBase58();
      const current = merged.get(key);
      merged.set(key, {
        pubkey: meta.pubkey,
        isSigner: Boolean(current?.isSigner || meta.isSigner),
        isWritable: Boolean(current?.isWritable || meta.isWritable),
      });
    }
  }
  return [...merged.values()];
}
