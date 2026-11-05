import { deriveJuniorCohortAddress } from "./constants.js";
import { oracleComputeBudgetInstructions, oraclePolicy, oracleReadAccounts, type OraclePolicyInput } from "./oracle.js";
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
import { type MarketLaunchConfig } from "./market-bootstrap.js";
import type { Market } from "./type-aliases.js";
import {
  deriveBorrowPositionAddress,
  deriveEventAuthorityAddress,
  deriveFutarchyAuthorityAddress,
  deriveLeverageCollateralVaultAddress,
  deriveLeverageDelegationAddress,
  deriveLeveragePositionAddress,
  deriveMarketAddress,
  deriveMarketCollateralVaultAddress,
  deriveMarketInterestVaultAddress,
  deriveMarketReserveVaultAddress,
  deriveTokenMetadataAddress,
  TOKEN_METADATA_PROGRAM_ID,
  deriveReferralAccrualAddress,
  deriveYieldAccountAddress,
  deriveYieldTransferHookValidationAddress,
  deriveHlpCohortAddress,
  deriveReferralPartnerAddress,
  type YieldTokenKind,
} from "./constants.js";
import {
  integerBN,
  type IntegerLike,
} from "./parameters.js";
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
  /** Explicit source policy for offline builders; async builders resolve it from the market. */
  oraclePolicy?: OraclePolicyInput;
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

type AnchorMethodBuilder = {
  accounts(accounts: DuskAccounts): AnchorMethodBuilder;
  remainingAccounts(accounts: AccountMeta[]): AnchorMethodBuilder;
  preInstructions(instructions: TransactionInstruction[]): AnchorMethodBuilder;
  instruction(): Promise<TransactionInstruction>;
  transaction(): Promise<Transaction>;
  rpc(): Promise<string>;
};

type AnchorMethods = Record<string, (...args: unknown[]) => AnchorMethodBuilder>;

export class DuskWrite {
  constructor(readonly program: Program<Dusk>) {}

  private fetchMarketAccount(market: PublicKey): Promise<unknown> {
    return (this.program as unknown as {
      account: { market: { fetch(address: PublicKey): Promise<unknown> } };
    }).account.market.fetch(market);
  }

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
    const remaining = [...(options.remainingAccounts ?? [])];
    for (const meta of oracleReadAccounts(options.oraclePolicy)) {
      if (!remaining.some(existing => existing.pubkey.equals(meta.pubkey))) remaining.push(meta);
    }
    if (remaining.length) builder = builder.remainingAccounts(remaining);
    const budgets = oracleComputeBudgetInstructions(options.oraclePolicy);
    if (budgets.length) builder = builder.preInstructions(budgets);
    return builder;
  }

  async instruction(
    name: DuskInstructionName,
    args?: DuskInstructionArgs,
    options?: DuskBuildOptions
  ): Promise<TransactionInstruction> {
    return this.builder(name, args, await this.withOraclePolicy(name, options)).instruction();
  }

  async transaction(
    name: DuskInstructionName,
    args?: DuskInstructionArgs,
    options?: DuskBuildOptions
  ): Promise<Transaction> {
    return this.builder(name, args, await this.withOraclePolicy(name, options)).transaction();
  }

  async rpc(name: DuskInstructionName, args?: DuskInstructionArgs, options?: DuskBuildOptions) {
    return this.builder(name, args, await this.withOraclePolicy(name, options)).rpc();
  }

  private async withOraclePolicy(name: DuskInstructionName, options: DuskBuildOptions = {}): Promise<DuskBuildOptions> {
    if (!ORACLE_READ_INSTRUCTIONS.has(name) || options.oraclePolicy) return options;
    const nested = options.accounts?.accounts as DuskAccounts | undefined;
    const preview = options.accounts?.preview as DuskAccounts | undefined;
    const market = options.accounts?.market ?? nested?.market ?? preview?.market;
    if (!market) return options;

    const state = await this.fetchMarketAccount(address(market as AddressLike)) as { oracle?: {policy: OraclePolicyInput} };
    return {...options, oraclePolicy: state.oracle?.policy};
  }

  /**
   * The hLP accounts an instruction must carry when hedged liquidity is live.
   *
   * Swap and openLeverage both require them, and their order is
   * consensus-visible, so both take them from here rather than each assembling
   * its own list.
   */
  /**
   * The hLP settlement prefix an instruction must carry when the market has
   * active hLP.
   *
   * Required by every leverage instruction that settles the market —
   * open, close, increase and decrease — not only by open. Omitting it fails
   * with `NotEnoughAccounts` at the point the program tries to read the
   * prefix, which names neither leverage nor hLP and reads as a malformed
   * call.
   */
  async hlpRemainingAccounts(
    marketAddress: AddressLike,
    marketSnapshot?: Market
  ): Promise<AccountMeta[]> {
    const market = address(marketAddress);
    const state = (marketSnapshot ?? await this.fetchMarketAccount(market)) as {
      srlpMint: AccountMeta["pubkey"];
      baseSide: { interestVault: AccountMeta["pubkey"]; assetMint: AccountMeta["pubkey"]; reserveVault: AccountMeta["pubkey"] };
      quoteSide: { interestVault: AccountMeta["pubkey"]; assetMint: AccountMeta["pubkey"]; reserveVault: AccountMeta["pubkey"] };
      baseHlpVault: {
        hlpSupply: { toString(): string };
        residualExposure: { toString(): string };
        srlpVault: AccountMeta["pubkey"];
      };
      quoteHlpVault: {
        hlpSupply: { toString(): string };
        residualExposure: { toString(): string };
        srlpVault: AccountMeta["pubkey"];
      };
    };
    const hlpActive =
      BigInt(state.baseHlpVault.hlpSupply.toString()) !== 0n ||
      BigInt(state.quoteHlpVault.hlpSupply.toString()) !== 0n ||
      BigInt(state.baseHlpVault.residualExposure.toString()) !== 0n ||
      BigInt(state.quoteHlpVault.residualExposure.toString()) !== 0n;
    if (!hlpActive) return [];
    return [
      { pubkey: state.srlpMint, isSigner: false, isWritable: true },
      { pubkey: state.baseHlpVault.srlpVault, isSigner: false, isWritable: true },
      { pubkey: state.quoteHlpVault.srlpVault, isSigner: false, isWritable: true },
      { pubkey: state.baseSide.interestVault, isSigner: false, isWritable: true },
      { pubkey: state.quoteSide.interestVault, isSigner: false, isWritable: true },
      // Fresh public-interest settlements rebalance both vaults. These keys
      // usually duplicate fixed swap accounts; repayments need them explicitly.
      { pubkey: state.baseSide.assetMint, isSigner: false, isWritable: false },
      { pubkey: state.quoteSide.assetMint, isSigner: false, isWritable: false },
      { pubkey: state.baseSide.reserveVault, isSigner: false, isWritable: true },
      { pubkey: state.quoteSide.reserveVault, isSigner: false, isWritable: true },
    ];
  }

  async swapBuilder(
    args: Record<string, unknown>,
    options: SwapBuildOptions
  ): Promise<AnchorMethodBuilder> {
    const market = address(options.market);
    const state = (await this.fetchMarketAccount(market)) as {
      srlpMint: AccountMeta["pubkey"];
      baseSide: { interestVault: AccountMeta["pubkey"] };
      quoteSide: { interestVault: AccountMeta["pubkey"] };
      baseHlpVault: {
        hlpSupply: { toString(): string };
        residualExposure: { toString(): string };
        srlpVault: AccountMeta["pubkey"];
      };
      quoteHlpVault: {
        hlpSupply: { toString(): string };
        residualExposure: { toString(): string };
        srlpVault: AccountMeta["pubkey"];
      };
    };
    const hlpActive =
      BigInt(state.baseHlpVault.hlpSupply.toString()) !== 0n ||
      BigInt(state.quoteHlpVault.hlpSupply.toString()) !== 0n ||
      BigInt(state.baseHlpVault.residualExposure.toString()) !== 0n ||
      BigInt(state.quoteHlpVault.residualExposure.toString()) !== 0n;
    const prefix: AccountMeta[] = hlpActive
      ? [
          { pubkey: state.srlpMint, isSigner: false, isWritable: true },
          { pubkey: state.baseHlpVault.srlpVault, isSigner: false, isWritable: true },
          { pubkey: state.quoteHlpVault.srlpVault, isSigner: false, isWritable: true },
          { pubkey: state.baseSide.interestVault, isSigner: false, isWritable: true },
          { pubkey: state.quoteSide.interestVault, isSigner: false, isWritable: true },
        ]
      : [];

    return this.builder("swap", args, {
      oraclePolicy: options.oraclePolicy ?? (state as typeof state & {oracle?: {policy: OraclePolicyInput}}).oracle?.policy,
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
        borrowAmount: integerBN(params.borrowAmount, "borrowAmount"),
        minDebtAmountOut: integerBN(
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
              deriveBorrowPositionAddress(market, owner, address(params.positionId))[0]
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
    const options = await this.withOraclePolicy("borrow", { accounts: {market: address(params.market)} });
    return new Transaction().add(...oracleComputeBudgetInstructions(options.oraclePolicy),
      await this.buildBorrowInstruction(params));
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

    if ("fundingAsset" in params && params.fundingAsset !== undefined && params.fundingAsset !== "debt")
      throw new Error("Leverage funding must use the debt asset; swap other tokens before opening");
    const positionId = address(params.positionId);
    const debtTokenProgram = await tokenProgramForMint(
      this.program.provider.connection,
      debtMint
    );

    const openArgs = {
        positionId,
        debtAsset: params.debtAsset === "quote" ? 1 : 0,
        marginAmount: integerBN(params.marginAmount, "marginAmount"),
        multiplierBps: integerBN(
          params.multiplierBps,
          "multiplierBps"
        ),
        minCollateralOut: integerBN(
          params.minCollateralOut,
          "minCollateralOut"
        ),
        referrer: null,
        positionOwner: null,
        limitPriceNad: integerBN(
          params.limitPriceNad ?? 0,
          "limitPriceNad"
        ),
      };
    return this.instruction(
      "openLeverage",
      openArgs,
      {
        accounts: {
          market,
          futarchyAuthority: deriveFutarchyAuthorityAddress()[0],
          owner,
          payer: address(params.payer ?? owner),
          leveragePosition: address(
            params.leveragePosition ??
              deriveLeveragePositionAddress(market, owner, positionId)[0]
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
    const options = await this.withOraclePolicy("openLeverage", { accounts: {market: address(params.market)} });
    return new Transaction().add(...oracleComputeBudgetInstructions(options.oraclePolicy),
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
        deriveLeveragePositionAddress(market, address(params.owner), address(params.positionId), address(params.namespaceAuthority ?? params.owner))[0]
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
        exactAssetIn: integerBN(params.exactAssetIn, "exactAssetIn"),
        minAssetOut: integerBN(params.minAssetOut, "minAssetOut"),
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
    /** Supply the retired mint's cohort when initializing claims after rotation. */
    retired?: boolean;
  }): Promise<TransactionInstruction> {
    const payer = address(params.payer);
    const owner = address(params.owner);
    const market = address(params.market);
    const lpMint = address(params.lpMint);
    const baseMint = address(params.baseMint);
    const quoteMint = address(params.quoteMint);
    const tokenKind = params.tokenKind === "srlp" || params.tokenKind === 0 ? { srlp: {} } : params.tokenKind === "junior" || params.tokenKind === 2 ? { junior: {} } : { hlp: {} };
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
      remainingAccounts: params.retired
        ? [{ pubkey: (params.tokenKind === "junior" || params.tokenKind === 2 ? deriveJuniorCohortAddress : deriveHlpCohortAddress)(market, lpMint, this.program.programId)[0], isSigner: false, isWritable: false }]
        : [],
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
    /** Pass this when composing market creation and hook setup in one transaction. */
    tokenKind?: YieldTokenKind;
  }): Promise<TransactionInstruction> {
    const lpMint = address(params.lpMint);
    const market = address(params.market);
    let tokenKind = params.tokenKind;
    if (tokenKind === undefined) {
      const state = await this.fetchMarketAccount(market) as Market;
      if (lpMint.equals(state.srlpMint)) tokenKind = "srlp";
      else if (lpMint.equals(state.juniorMint)) tokenKind = "junior";
      else if (lpMint.equals(state.baseSide.hlpMint) || lpMint.equals(state.quoteSide.hlpMint)) tokenKind = "hlp";
      else throw new Error("LP mint does not belong to the market");
    }
    const isHlp = tokenKind === "hlp" || tokenKind === 1;
    return this.instruction("initializeLpTransferHook" as DuskInstructionName, undefined, {
      accounts: {
        payer: address(params.payer),
        market,
        lpMint,
        validationAccount: deriveYieldTransferHookValidationAddress(lpMint, this.program.programId)[0],
        systemProgram: SystemProgram.programId,
      },
      remainingAccounts: isHlp
        ? [{ pubkey: deriveHlpCohortAddress(market, lpMint, this.program.programId)[0], isSigner: false, isWritable: true }]
        : [],
    });
  }

  async initializeLpTransferHookTransaction(
    params: Parameters<DuskWrite["initializeLpTransferHookInstruction"]>[0]
  ): Promise<Transaction> {
    return new Transaction().add(await this.initializeLpTransferHookInstruction(params));
  }

  /**
   * Bring a market into existence.
   *
   * The three LP mints must already exist and carry the Dusk transfer hook —
   * see `createHookedLpMintInstructions`. Every PDA is derived here rather
   * than left to Anchor, because the market address depends on `paramsHash`,
   * which is an argument rather than an account, and the vaults hang off the
   * market in turn.
   *
   * `teamTreasury` and its wrapped-SOL account come from the futarchy
   * authority, which must already be initialized on the deployment.
   */
  async initializeMarketInstruction(params: {
    payer: AddressLike;
    baseMint: AddressLike;
    quoteMint: AddressLike;
    srlpMint: AddressLike;
    baseHlpMint: AddressLike;
    quoteHlpMint: AddressLike;
    teamTreasury: AddressLike;
    teamTreasuryWsolAccount: AddressLike;
    paramsHash: Uint8Array | Buffer | number[];
    config: MarketLaunchConfig;
    /** Share of base interest retained by the operator, 0–5000 bps. */
    operatorInterestShareBps?: number;
    /** Installed atomically at creation. Omit or pass null for native pricing.
     * Guided first liquidity derives its ratio from the validated current feed. */
    oracle?: OraclePolicyInput | null;
    /** Native launch-adapter price in NAD. Guided creation requires zero. */
    bootstrapPriceNad?: IntegerLike;
    launchFeeProgressOffset?: number;
    lpMetadata: {
      srlp: { name: string; symbol: string; uri: string };
      baseHlp: { name: string; symbol: string; uri: string };
      quoteHlp: { name: string; symbol: string; uri: string };
    };
  }): Promise<TransactionInstruction> {
    const operatorShare=params.operatorInterestShareBps??0;
    if(!Number.isInteger(operatorShare)||operatorShare<0||operatorShare>5_000)throw new Error("operatorInterestShareBps must be 0–5000");
    const baseMint = address(params.baseMint);
    const quoteMint = address(params.quoteMint);
    const paramsHash = Uint8Array.from(params.paramsHash);
    if (paramsHash.length !== 32) {
      throw new Error(`paramsHash must be 32 bytes, received ${paramsHash.length}`);
    }
    const [market] = deriveMarketAddress(baseMint, quoteMint, paramsHash, this.program.programId);
    const oracle = params.oracle == null ? null : oraclePolicy(params.oracle);
    const bootstrapPriceNad = integerBN(params.bootstrapPriceNad ?? 0, "bootstrapPriceNad");
    if (oracle && oracle.lambdaBps > 0 && !bootstrapPriceNad.isZero())
      throw new Error("Oracle-guided markets derive the initial price from the feed; omit bootstrapPriceNad");
    return this.instruction(
      "initializeMarket" as DuskInstructionName,
      {
        operatorInterestShareBps:operatorShare,
        config: anchorMarketConfig(params.config),
        oracle,
        paramsHash: [...paramsHash],
        bootstrapPriceNad,
        launchFeeProgressOffset: params.launchFeeProgressOffset ?? 0,
        srlpMetadata: params.lpMetadata.srlp,
        baseHlpMetadata: params.lpMetadata.baseHlp,
        quoteHlpMetadata: params.lpMetadata.quoteHlp,
      },
      {
        accounts: {
          payer: address(params.payer),
          baseMint,
          quoteMint,
          srlpMint: address(params.srlpMint),
          baseHlpMint: address(params.baseHlpMint),
          quoteHlpMint: address(params.quoteHlpMint),
          srlpTokenMetadata: deriveTokenMetadataAddress(address(params.srlpMint))[0],
          baseHlpTokenMetadata: deriveTokenMetadataAddress(address(params.baseHlpMint))[0],
          quoteHlpTokenMetadata: deriveTokenMetadataAddress(address(params.quoteHlpMint))[0],
          market,
          futarchyAuthority: deriveFutarchyAuthorityAddress()[0],
          baseReserveVault: deriveMarketReserveVaultAddress(market, baseMint)[0],
          quoteReserveVault: deriveMarketReserveVaultAddress(market, quoteMint)[0],
          baseCollateralVault: deriveMarketCollateralVaultAddress(market, baseMint)[0],
          quoteCollateralVault: deriveMarketCollateralVaultAddress(market, quoteMint)[0],
          baseInterestVault: deriveMarketInterestVaultAddress(market, baseMint)[0],
          quoteInterestVault: deriveMarketInterestVaultAddress(market, quoteMint)[0],
          teamTreasury: address(params.teamTreasury),
          teamTreasuryWsolAccount: address(params.teamTreasuryWsolAccount),
          systemProgram: SystemProgram.programId,
          tokenProgram: TOKEN_PROGRAM_ID,
          token2022Program: TOKEN_2022_PROGRAM_ID,
          sysvarInstructions: SYSVAR_INSTRUCTIONS_PUBKEY,
          tokenMetadataProgram: TOKEN_METADATA_PROGRAM_ID,
        },
      }
    );
  }

  async initializeMarketTransaction(
    params: Parameters<DuskWrite["initializeMarketInstruction"]>[0]
  ): Promise<Transaction> {
    return new Transaction().add(await this.initializeMarketInstruction(params));
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
      oraclePolicy: options.oraclePolicy,
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

  /** Begin a fresh hLP receipt cohort after the current vault has terminally closed. */
  rotateHlpCohortInstruction(params: {
    payer: AddressLike;
    market: AddressLike;
    oldHlpMint: AddressLike;
    newHlpMint: AddressLike;
    targetAsset: "base" | "quote";
    metadata: { name: string; symbol: string; uri: string };
  }): Promise<TransactionInstruction> {
    const market = address(params.market);
    const oldHlpMint = address(params.oldHlpMint);
    const newHlpMint = address(params.newHlpMint);
    return this.instruction("rotateHlpCohort" as DuskInstructionName, {
      targetAsset: params.targetAsset === "base" ? 0 : 1,
      metadata: params.metadata,
    }, {
      accounts: {
        payer: address(params.payer),
        market,
        oldHlpMint,
        newHlpMint,
        oldCohort: deriveHlpCohortAddress(market, oldHlpMint, this.program.programId)[0],
        newTokenMetadata: deriveTokenMetadataAddress(newHlpMint)[0],
        sysvarInstructions: SYSVAR_INSTRUCTIONS_PUBKEY,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        tokenMetadataProgram: TOKEN_METADATA_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      },
    });
  }

  /** Burn old hLP receipts after terminal closure while preserving earned yield. */
  burnRetiredHlpReceiptInstruction(params: {
    market: AddressLike;
    owner: AddressLike;
    retiredMint: AddressLike;
    baseMint: AddressLike;
    quoteMint: AddressLike;
    amount: IntegerLike;
    ownerHlpAccount?: AddressLike;
  }): Promise<TransactionInstruction> {
    const market = address(params.market);
    const owner = address(params.owner);
    const retiredMint = address(params.retiredMint);
    return this.instruction("burnRetiredHlpReceipt" as DuskInstructionName, {
      amount: integerBN(params.amount),
    }, {
      accounts: {
        market,
        owner,
        retiredMint,
        cohort: deriveHlpCohortAddress(market, retiredMint, this.program.programId)[0],
        ownerHlpAccount: params.ownerHlpAccount
          ? address(params.ownerHlpAccount)
          : getAssociatedTokenAddressSync(retiredMint, owner, true, TOKEN_2022_PROGRAM_ID),
        baseYieldAccount: deriveYieldAccountAddress(
          market, owner, retiredMint, address(params.baseMint), "hlp", this.program.programId
        )[0],
        quoteYieldAccount: deriveYieldAccountAddress(
          market, owner, retiredMint, address(params.quoteMint), "hlp", this.program.programId
        )[0],
        token2022Program: TOKEN_2022_PROGRAM_ID,
      },
    });
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
    const referralAccrual = (this.program as unknown as {
      account: {
        referralAccrual: {
          fetch(address: PublicKey): Promise<{ amount: { toString(): string } }>;
        };
      };
    }).account.referralAccrual;
    const [accrual, mint] = await Promise.all([
      referralAccrual.fetch(referral.referralAccrual),
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
    return this.instruction(
      "depositCollateral" as DuskInstructionName,
      { positionId, depositAmount: integerBN(params.depositAmount, "depositAmount") },
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
              deriveBorrowPositionAddress(market, owner, positionId)[0]
          ),
          tokenProgram: TOKEN_PROGRAM_ID,
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
        remainingAccounts: params.remainingAccounts,
      }
    );
  }

  /** Donate to an existing borrow position. `owner` is the token payer. */
  async donateCollateralInstruction(params: DepositCollateralParams): Promise<TransactionInstruction> {
    const market = address(params.market);
    const assetMint = address(params.assetMint);
    return this.instruction("donateCollateral" as DuskInstructionName,
      { depositAmount: integerBN(params.depositAmount, "depositAmount") }, {
        accounts: {
          market, owner: address(params.owner), assetMint,
          collateralVault: address(params.collateralVault ?? deriveMarketCollateralVaultAddress(market, assetMint)[0]),
          ownerAssetAccount: address(params.ownerAssetAccount),
          borrowPosition: address(params.borrowPosition ?? deriveBorrowPositionAddress(market, address(params.positionOwner ?? params.owner), address(params.positionId))[0]),
          tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID,
        }, remainingAccounts: params.remainingAccounts,
      });
  }

  /** Sweep both collateral balances and close a debt-free borrow position. */
  async withdrawAllCollateralInstruction(params: WithdrawAllCollateralParams): Promise<TransactionInstruction> {
    const market = address(params.market);
    const baseMint = address(params.baseMint);
    const quoteMint = address(params.quoteMint);
    return this.instruction("withdrawAllCollateral" as DuskInstructionName, {
      minBaseOut: integerBN(params.minBaseOut, "minBaseOut"),
      minQuoteOut: integerBN(params.minQuoteOut, "minQuoteOut"),
    }, { accounts: {
      market, owner: address(params.owner), baseMint, quoteMint,
      borrowPosition: address(params.borrowPosition ?? deriveBorrowPositionAddress(market, address(params.owner), address(params.positionId))[0]),
      baseCollateralVault: deriveMarketCollateralVaultAddress(market, baseMint)[0],
      quoteCollateralVault: deriveMarketCollateralVaultAddress(market, quoteMint)[0],
      ownerBaseAccount: address(params.ownerBaseAccount), ownerQuoteAccount: address(params.ownerQuoteAccount),
      tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID,
    }, remainingAccounts: params.remainingAccounts });
  }

  /** Return all collateral after a complete leverage repayment and close the position. */
  async withdrawRepaidLeverageInstruction(params: WithdrawRepaidLeverageParams): Promise<TransactionInstruction> {
    const market = address(params.market);
    const collateralMint = address(params.collateralMint);
    return this.instruction("withdrawRepaidLeverage" as DuskInstructionName,
      { minCollateralOut: integerBN(params.minCollateralOut, "minCollateralOut") }, {
        accounts: {
          market, owner: address(params.owner), collateralMint,
          leveragePosition: address(params.leveragePosition ?? deriveLeveragePositionAddress(market, address(params.owner), address(params.positionId), address(params.namespaceAuthority ?? params.owner))[0]),
          collateralVault: deriveLeverageCollateralVaultAddress(market, collateralMint)[0],
          ownerCollateralAccount: address(params.ownerCollateralAccount),
          tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID,
        }, remainingAccounts: params.remainingAccounts,
      });
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
    return this.instruction(
      "withdrawCollateral" as DuskInstructionName,
      {
        withdrawAmount: integerBN(params.withdrawAmount, "withdrawAmount"),
        minAssetAmountOut: integerBN(params.minAssetAmountOut, "minAssetAmountOut"),
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
              deriveBorrowPositionAddress(market, owner, positionId)[0]
          ),
          tokenProgram: TOKEN_PROGRAM_ID,
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
   * Repay anyone’s borrowed debt using `owner`’s tokens, without collateral rights.
   * Pass the wallet balance as `repayAmount` to clear a
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
    return this.instruction(
      "repay" as DuskInstructionName,
      { repayAmount: integerBN(params.repayAmount, "repayAmount") },
      {
        accounts: {
          market,
          futarchyAuthority: deriveFutarchyAuthorityAddress()[0],
          owner,
          debtAssetMint,
          collateralAssetMint: address(params.collateralAssetMint),
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
              deriveBorrowPositionAddress(market, address(params.positionOwner ?? params.owner), positionId)[0]
          ),
          referralPartner,
          referralAccrual: referralPartner
            ? deriveReferralAccrualAddress(
                referralPartner,
                market,
                debtAssetMint
              )[0]
            : null,
          tokenProgram: TOKEN_PROGRAM_ID,
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
        remainingAccounts: [...(await this.hlpRemainingAccounts(market)), ...(params.remainingAccounts ?? [])],
      }
    );
  }

  async repayTransaction(params: RepayParams): Promise<Transaction> {
    return new Transaction().add(await this.repayInstruction(params));
  }

  /**
   * Add balanced srLP liquidity. `minSrlpAmount` is the caller's slippage floor;
   * the program rejects the deposit rather than minting fewer shares.
   */
  async addLiquidityInstruction(
    params: AddLiquidityParams
  ): Promise<TransactionInstruction> {
    const common = await this.resolveSrlpAccounts(params);
    return this.instruction(
      "addLiquidity" as DuskInstructionName,
      {
        baseDepositAmount: integerBN(
          params.baseDepositAmount,
          "baseDepositAmount"
        ),
        quoteDepositAmount: integerBN(
          params.quoteDepositAmount,
          "quoteDepositAmount"
        ),
        minSrlpAmount: integerBN(params.minSrlpAmount, "minSrlpAmount"),
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
   * Burn srLP for the underlying pair. Both minimums are enforced by the
   * program, so a partially-filled withdrawal fails instead of silently
   * returning less of one side.
   */
  async removeLiquidityInstruction(
    params: RemoveLiquidityParams
  ): Promise<TransactionInstruction> {
    const common = await this.resolveSrlpAccounts(params);
    return this.instruction(
      "removeLiquidity" as DuskInstructionName,
      {
        srlpAmount: integerBN(params.srlpAmount, "srlpAmount"),
        minBaseAmountOut: integerBN(
          params.minBaseAmountOut,
          "minBaseAmountOut"
        ),
        minQuoteAmountOut: integerBN(
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
   * Accounts shared by both balanced srLP instructions. Reserve vaults and srLP
   * yield accounts derive from the market; token programs resolve per mint so
   * a Token-2022 side needs no caller branching.
   */
  /** Borrow more against an open position and receive collateral. */
  async increaseLeverageInstruction(params: IncreaseLeverageParams): Promise<TransactionInstruction> {
    return this.fundLeverageInstruction(params, 0n);
  }

  /** Add debt-token equity and optional borrowing to the same position. */
  async addToLeverageInstruction(params: AddToLeverageParams): Promise<TransactionInstruction> {
    if (BigInt(params.marginAmount.toString()) <= 0n) throw new RangeError("marginAmount must be positive");
    return this.fundLeverageInstruction(params, params.marginAmount, params.ownerDebtAccount);
  }

  async addToLeverageTransaction(params: AddToLeverageParams): Promise<Transaction> {
    return new Transaction().add(await this.addToLeverageInstruction(params));
  }

  private async fundLeverageInstruction(
    params: IncreaseLeverageParams, marginAmount: RawAmount, ownerDebtAccount?: AddressLike
  ): Promise<TransactionInstruction> {
    const core = await this.resolveLeverageAccounts(params);
    return this.instruction(
      "increaseLeverage" as DuskInstructionName,
      {
        debtAsset: marketAssetIndex(params.debtAsset),
        debtAmount: integerBN(params.debtAmount, "debtAmount"),
        marginAmount: integerBN(marginAmount, "marginAmount"),
        minCollateralOut: integerBN(
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
          ownerDebtAccount: ownerDebtAccount ? address(ownerDebtAccount) : null,
          instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY,
          tokenProgram: core.tokenProgram,
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
        remainingAccounts: [
          ...(await this.hlpRemainingAccounts(core.market)),
          ...(params.remainingAccounts ?? []),
        ],
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
        collateralAmount: integerBN(
          params.collateralAmount,
          "collateralAmount"
        ),
        minRepayOut: integerBN(params.minRepayOut, "minRepayOut"),
        maxSurplusOut: integerBN(params.maxSurplusOut ?? 0n, "maxSurplusOut"),
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
          ownerDebtAccount: address(params.ownerDebtAccount),
          leverageCollateralVault: core.leverageCollateralVault,
          referralPartner: core.referralPartner,
          referralAccrual: core.referralAccrual,
          owner: core.owner,
          instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY,
          tokenProgram: core.tokenProgram,
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
        remainingAccounts: [
          ...(await this.hlpRemainingAccounts(core.market)),
          ...(params.remainingAccounts ?? []),
        ],
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

  /** Repay anyone’s leverage debt from `owner`’s tokens; collateral stays with the position owner. */
  async addLeverageMarginInstruction(params: LeverageMarginParams): Promise<TransactionInstruction> {
    return this.repayLeverageInstruction(params);
  }

  /** Permissionless repayment without an unnecessary collateral-sale quote. */
  async repayLeverageInstruction(params: LeverageMarginParams): Promise<TransactionInstruction> {
    return this.leverageRepaymentInstruction(params, "repayLeverage");
  }

  private async leverageRepaymentInstruction(
    params: LeverageMarginParams, method: "addLeverageMargin" | "repayLeverage"
  ): Promise<TransactionInstruction> {
    const core = await this.resolveLeverageAccounts(params);
    return this.instruction(
      method as DuskInstructionName,
      {
        debtAsset: marketAssetIndex(params.debtAsset),
        amount: integerBN(params.amount, "amount"),
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
          debtInterestVault: core.debtInterestVault,
          ownerDebtAccount: address(params.ownerDebtAccount),
          referralPartner: core.referralPartner,
          referralAccrual: core.referralAccrual,
          owner: core.owner,
          tokenProgram: core.tokenProgram,
          token2022Program: TOKEN_2022_PROGRAM_ID,
        },
        remainingAccounts: [...(await this.hlpRemainingAccounts(core.market)), ...(params.remainingAccounts ?? [])],
      }
    );
  }

  async repayLeverageTransaction(params: LeverageMarginParams): Promise<Transaction> {
    return new Transaction().add(await this.repayLeverageInstruction(params));
  }

  async addLeverageMarginTransaction(
    params: LeverageMarginParams
  ): Promise<Transaction> {
    return new Transaction().add(
      await this.repayLeverageInstruction(params)
    );
  }

  /**
   * Owner-signed proportional partial/full close. Delegated closes use the delegate client.
   */
  async closeLeverageInstruction(
    params: CloseLeverageParams,
    marketSnapshot?: Market
  ): Promise<TransactionInstruction> {
    if ("collateralFunded" in params && params.collateralFunded === true)
      throw new Error("Collateral-funded leverage closes are no longer supported");
    const closeBps = params.closeBps ?? 10_000;
    if (!Number.isSafeInteger(closeBps) || closeBps <= 0 || closeBps > 10_000)
      throw new RangeError("closeBps must be between 1 and 10000");
    const core = await this.resolveLeverageAccounts(params);
    return this.instruction(
      "closeLeverage",
      {
        debtAsset: marketAssetIndex(params.debtAsset),
        minAmountOut: integerBN(params.minAmountOut, "minAmountOut"),
        closeBps,
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
          delegateFeeRecipient: params.delegateFeeRecipient
            ? address(params.delegateFeeRecipient)
            : null,
          delegateExecutorAccount: params.delegateExecutorAccount
            ? address(params.delegateExecutorAccount)
            : null,
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
        remainingAccounts: [
          ...(await this.hlpRemainingAccounts(core.market, marketSnapshot)),
          ...(params.remainingAccounts ?? []),
        ],
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
    return {
      market,
      futarchyAuthority: deriveFutarchyAuthorityAddress()[0],
      positionOwner,
      owner: address(params.owner ?? params.positionOwner),
      leveragePosition: address(
        params.leveragePosition ??
          deriveLeveragePositionAddress(market, positionOwner, positionId, address(params.namespaceAuthority ?? params.positionOwner))[0]
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
      tokenProgram: TOKEN_PROGRAM_ID,
    };
  }

  private async resolveSrlpAccounts(params: SrlpLiquidityAccounts) {
    const market = address(params.market);
    const owner = address(params.owner);
    const baseMint = address(params.baseMint);
    const quoteMint = address(params.quoteMint);
    const srlpMint = address(params.srlpMint);
    return {
      market,
      owner,
      baseMint,
      quoteMint,
      srlpMint,
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
      ownerSrlpAccount: address(params.ownerSrlpAccount),
      baseYieldAccount: address(
        params.baseYieldAccount ??
          deriveYieldAccountAddress(
            market,
            owner,
            srlpMint,
            baseMint,
            "srlp",
            this.program.programId
          )[0]
      ),
      quoteYieldAccount: address(
        params.quoteYieldAccount ??
          deriveYieldAccountAddress(
            market,
            owner,
            srlpMint,
            quoteMint,
            "srlp",
            this.program.programId
          )[0]
      ),
      tokenProgram: TOKEN_PROGRAM_ID,
      token2022Program: TOKEN_2022_PROGRAM_ID,
    };
  }
}

/** Raw base-unit amount. Accepts bigint to keep callers off floating point. */
export type RawAmount = bigint | number | string;

interface LendingPositionAccounts {
  market: AddressLike;
  owner: AddressLike;
  /** Beneficiary when a different wallet pays for this position. */
  positionOwner?: AddressLike;
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
  collateralAssetMint: AddressLike;
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
  /** Signer that originally funded the position, if different from its owner. */
  namespaceAuthority?: AddressLike;
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

export interface AddToLeverageParams extends IncreaseLeverageParams {
  /** Gross wallet funding in the borrowed token. */
  marginAmount: RawAmount;
  ownerDebtAccount: AddressLike;
}

export interface DecreaseLeverageParams extends LeverageAccounts {
  collateralAmount: RawAmount;
  minRepayOut: RawAmount;
  /** Owner-owned debt-token account for unused repayment output. */
  ownerDebtAccount: AddressLike;
  /** Gross surplus limit; defaults to zero. Use a bounded value for 1x rounding. */
  maxSurplusOut?: RawAmount;
}

export interface LeverageMarginParams extends LeverageAccounts {
  amount: RawAmount;
  ownerDebtAccount: AddressLike;
}

export interface CloseLeverageParams extends LeverageAccounts {
  /** Minimum net wallet payout after all transfer fees. */
  minAmountOut: RawAmount;
  /** Percentage of the entire position to remove. Default 10000 closes all. */
  closeBps?: number;
  /** Payout account owned by the position owner. */
  ownerDebtAccount: AddressLike;
  /** Required for delegated closes; the two accounts receive bounded fees. */
  delegateFeeRecipient?: AddressLike | null;
  delegateExecutorAccount?: AddressLike | null;
  /** Delegated settlement; omit these for an owner-signed close. */
  leverageDelegation?: AddressLike | null;
  delegatedProgram?: AddressLike | null;
  authority?: AddressLike;
}

interface SrlpLiquidityAccounts {
  market: AddressLike;
  owner: AddressLike;
  baseMint: AddressLike;
  quoteMint: AddressLike;
  srlpMint: AddressLike;
  ownerBaseAccount: AddressLike;
  ownerQuoteAccount: AddressLike;
  ownerSrlpAccount: AddressLike;
  baseReserveVault?: AddressLike;
  quoteReserveVault?: AddressLike;
  baseYieldAccount?: AddressLike;
  quoteYieldAccount?: AddressLike;
  remainingAccounts?: AccountMeta[];
}

/** Which side of the market a leverage position borrows. */
export type LeverageDebtAsset = "base" | "quote";

/** Opening leverage funded in the debt asset; margin plus borrowing buys collateral. */
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
  namespaceAuthority?: AddressLike;
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

export interface AddLiquidityParams extends SrlpLiquidityAccounts {
  /** Maximum gross spend. Guided first deposits pair at the current validated
   * oracle price; unused amounts remain with the owner. Native seeding uses
   * both amounts to establish the initial ratio. */
  baseDepositAmount: RawAmount;
  quoteDepositAmount: RawAmount;
  /** Slippage floor on minted srLP shares. */
  minSrlpAmount: RawAmount;
}

export interface RemoveLiquidityParams extends SrlpLiquidityAccounts {
  srlpAmount: RawAmount;
  minBaseAmountOut: RawAmount;
  minQuoteAmountOut: RawAmount;
}

/**
 * Widen a launch config into the shape Anchor expects: every `u64` as a BN,
 * every `u16` left as a number. Written out field by field rather than mapped,
 * so a field added to the program fails to compile here instead of silently
 * going missing.
 */
function anchorMarketConfig(config: MarketLaunchConfig): Record<string, unknown> {
  const bn = (value: IntegerLike, label: string) => integerBN(value, label);
  if (bn(config.amm.peakAmplificationNad as IntegerLike, "peakAmplificationNad").gtn(1_000_000_000)
      && bn(config.amm.adjustmentStepNad as IntegerLike, "adjustmentStepNad").isZero()) {
    throw new Error("concentrated markets require an enabled center controller");
  }
  return {
    swapFeeBps: config.swapFeeBps,
    divergenceFeeShareCapBps: config.divergenceFeeShareCapBps,
    volatilityFeeShareCapBps: config.volatilityFeeShareCapBps,
    targetHlpLeverageBps: config.targetHlpLeverageBps,
    settlementDivergenceBps: config.settlementDivergenceBps,
    emaHalfLifeMs: bn(config.emaHalfLifeMs, "emaHalfLifeMs"),
    directionalEmaHalfLifeMs: bn(
      config.directionalEmaHalfLifeMs,
      "directionalEmaHalfLifeMs"
    ),
    curveDepthEmaHalfLifeMs: bn(config.curveDepthEmaHalfLifeMs, "curveDepthEmaHalfLifeMs"),
    maxDailyBorrowBps: config.maxDailyBorrowBps,
    globalHealthContributionCapBps: config.globalHealthContributionCapBps,
    borrowMarketHealthFloorBps: config.borrowMarketHealthFloorBps,
    amm: anchorIntegerFields(config.amm, AMM_U64_FIELDS),
    irm: anchorIntegerFields(config.irm, IRM_U64_FIELDS),
    startTime: bn(config.startTime, "startTime"),
  };
}

const AMM_U64_FIELDS = [
  "peakAmplificationNad",
  "centerEmaHalfLifeMs",
  "volatilityHalfLifeMs",
  "adjustmentStepNad",
  "volatilityShockCapNad",
  "volatilityCapNad",
  "divergenceFeeCoefficientNad",
  "volatilityFeeCoefficientNad",
  "launchFeeDurationSeconds",
  "launchRateLimitReferenceNad",
  "launchRateLimitDurationSeconds",
] as const;

const IRM_U64_FIELDS = ["lowerSignalNad", "upperSignalNad", "halfLifeMs", "initialAprNad"] as const;

function anchorIntegerFields(
  source: Record<string, unknown>,
  wide: readonly string[]
): Record<string, unknown> {
  const out: Record<string, unknown> = { ...source };
  for (const field of wide) {
    if (out[field] === undefined) continue;
    out[field] = integerBN(out[field] as IntegerLike, field);
  }
  return out;
}

function normalizeArgs(args: DuskInstructionArgs): unknown[] {
  if (args === undefined) {
    return [];
  }
  return Array.isArray(args) ? args : [args];
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

export interface WithdrawAllCollateralParams extends LendingPositionAccounts {
  baseMint: AddressLike;
  quoteMint: AddressLike;
  ownerBaseAccount: AddressLike;
  ownerQuoteAccount: AddressLike;
  minBaseOut: RawAmount;
  minQuoteOut: RawAmount;
}

export interface WithdrawRepaidLeverageParams {
  market: AddressLike;
  owner: AddressLike;
  namespaceAuthority?: AddressLike;
  positionId: AddressLike;
  leveragePosition?: AddressLike;
  collateralMint: AddressLike;
  ownerCollateralAccount: AddressLike;
  minCollateralOut: RawAmount;
  remainingAccounts?: AccountMeta[];
}

// Kept in step with the on-chain instruction binding boundary.
const ORACLE_READ_INSTRUCTIONS = new Set<string>([
  "addLiquidity",
  "openLiquidityGates",
  "removeLiquidity",
  "fundInterest",
  "harvest",
  "swap",
  "previewHlpFlash",
  "beginHlpFlash",
  "settleHlpFlash",
  "closeInsolventHlp",
  "beginFlashLiquidation",
  "previewFlashLiquidation",
  "previewFlashAmmLiquidation",
  "emergencyLiquidation",
  "previewEmergencyLiquidation",
  "previewLiquidationHealth",
  "settleFlashLiquidation",
  "depositCollateral",
  "withdrawCollateral",
  "borrow",
  "repay",
  "donateCollateral",
  "openLeverage",
  "closeLeverage",
  "delegatedCloseLeverage",
  "increaseLeverage",
  "decreaseLeverage",
  "addLeverageMargin",
  "repayLeverage",
  "previewHlpDepositCapacity",
  "previewMarket",
  "previewHlpOrderTrigger",
  "previewAddLiquidity",
  "previewSwap",
  "previewBorrowCapacity",
  "previewBorrowPositionCapacity",
  "previewBorrowPosition",
  "depositSingleSided",
  "withdrawSingleSided",
  "executeOperatorChange",
]);
