import type { IdlTypes, Program } from "@coral-xyz/anchor";
import { createAssociatedTokenAccountIdempotentInstruction, getAssociatedTokenAddressSync, TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID } from "@solana/spl-token";
import { ComputeBudgetProgram, PublicKey, SystemProgram, SYSVAR_INSTRUCTIONS_PUBKEY, type AccountMeta, type TransactionInstruction } from "@solana/web3.js";
import { address, type AddressLike } from "./address.js";
import { deriveFutarchyAuthorityAddress } from "./constants.js";
import { DuskGet } from "./get.js";
import { governanceIntegerBN, type GovernanceIntegerLike } from "./governance.js";
import { previewReturnDataBytes } from "./preview.js";
import { tokenProgramForMint } from "./referral.js";
import type { Dusk as DuskIdl } from "./types_v2.js";
import { DuskWrite, type HlpLiquidityBuildOptions } from "./write.js";

export type HlpFlashQuote = IdlTypes<DuskIdl>["hlpFlashQuote"];
export interface HlpRecoveryParams { market: AddressLike; targetAsset: "base" | "quote" }
export interface HlpFlashRouteContext {
  quote: HlpFlashQuote;
  session: PublicKey;
  repaymentVault: PublicKey;
  buyerCollateralAccount: PublicKey;
  buyerDebtAccount: PublicKey;
  collateralMint: PublicKey;
  debtMint: PublicKey;
  collateralTokenProgram: PublicKey;
  debtTokenProgram: PublicKey;
  /** Net spendable atoms required in repaymentVault. A route must gross up
   * its incoming transfer; this already includes the vault-to-reserve fee. */
  requiredPaymentCredit: bigint;
}
export interface HlpFlashParams extends HlpRecoveryParams {
  buyer: AddressLike;
  /** Explicit opt-in to closing an insolvent vault's entire backing. */
  allowFull?: boolean;
  maxPayment?: GovernanceIntegerLike;
  maxCollateralDebit?: GovernanceIntegerLike;
  minCollateralCredit?: GovernanceIntegerLike;
  maxInsuranceDraw?: GovernanceIntegerLike;
  maxSocializedLoss?: GovernanceIntegerLike;
  prefixInstructions?: readonly TransactionInstruction[];
  settleRemainingAccounts?: readonly AccountMeta[];
  route: (context: HlpFlashRouteContext) => Promise<readonly TransactionInstruction[]> | readonly TransactionInstruction[];
  /** Append an owner-signed withdrawal after funded recovery or terminal
   * settlement. A terminal close pays zero principal. */
  exit?: HlpRecoveryExit;
}

export interface HlpRecoveryExit {
  hlpAmount: GovernanceIntegerLike;
  minTargetAmountOut?: GovernanceIntegerLike;
  options: HlpLiquidityBuildOptions;
}

export interface HlpNativeCloseAndExitParams extends HlpRecoveryParams {
  exit: HlpRecoveryExit;
  maxInsuranceDraw?: GovernanceIntegerLike;
  maxSocializedLoss?: GovernanceIntegerLike;
  minCallerBountyOut?: GovernanceIntegerLike;
  closeRemainingAccounts?: readonly AccountMeta[];
}

/** Atomic vault recovery. A solvent funding gap can be rescued before the
 * native critical-stress boundary; a full quote still requires insolvency.
 * Keeps a fixed payment obligation and permits an external route, Dusk swap,
 * or inventory payment. Never signs or submits. */
export class DuskHlpLiquidations {
  readonly write: DuskWrite;
  readonly get: DuskGet;
  constructor(readonly program: Program<DuskIdl>, feePayer?: AddressLike) {
    this.write = new DuskWrite(program);
    this.get = new DuskGet(program, feePayer);
  }

  private async resolve(params: HlpRecoveryParams) {
    if (params.targetAsset !== "base" && params.targetAsset !== "quote") throw new Error("Invalid hLP target asset");
    const market = address(params.market), target = params.targetAsset === "base" ? 0 : 1;
    const state = await this.program.account.market.fetch(market);
    const collateral = target === 0 ? state.baseSide : state.quoteSide;
    const debt = target === 0 ? state.quoteSide : state.baseSide;
    const futarchyAuthority = deriveFutarchyAuthorityAddress(this.program.programId)[0];
    return { market, target, state, collateral, debt, futarchyAuthority };
  }

  /** Uses the program's full-value eligibility, EMA payment quote, transfer
   * fees and affordable incentive. A positive funding gap or frozen vault can
   * quote a solvent rescue; inspect quote.full before building a transaction.
   * It does not reserve liquidity or payment. */
  async preview(params: HlpRecoveryParams): Promise<HlpFlashQuote> {
    const r = await this.resolve(params);
    const instruction = await this.write.instruction("previewHlpFlash", [r.target], {
      accounts: { market: r.market, futarchyAuthority: r.futarchyAuthority, targetMint: r.collateral.assetMint, borrowedMint: r.debt.assetMint },
    });
    return this.program.coder.types.decode("hlpFlashQuote", previewReturnDataBytes(await this.get.simulateReturnData(instruction))) as HlpFlashQuote;
  }

  async isFrozen(params: HlpRecoveryParams): Promise<boolean> {
    const r = await this.resolve(params);
    return (r.state.amm.frozenHlpMask & (r.target === 0 ? 1 : 2)) !== 0;
  }

  private async exitInstruction(r: Awaited<ReturnType<DuskHlpLiquidations["resolve"]>>, exit: HlpRecoveryExit) {
    if (!address(exit.options.market).equals(r.market)) throw new Error("Exit market differs from liquidation market");
    if (!address(exit.options.targetHlpMint).equals(r.collateral.hlpMint)) throw new Error("Exit hLP mint differs from liquidation target");
    if (!address(exit.options.baseMint).equals(r.state.baseSide.assetMint) ||
        !address(exit.options.quoteMint).equals(r.state.quoteSide.assetMint)) throw new Error("Exit asset mints differ from market");
    const built = await this.write.withdrawSingleSided({
      hlpAmount: governanceIntegerBN(exit.hlpAmount, "hlpAmount"),
      minTargetAmountOut: governanceIntegerBN(exit.minTargetAmountOut ?? 0, "minTargetAmountOut"),
    }, exit.options);
    return built;
  }

  /** One signed transaction: permissionless native insurance/writeoff close,
   * then the holder burns receipts. Only succeeds if both spot and symmetric
   * EMA value show terminal insolvency. The receipt burn pays zero principal. */
  async buildNativeCloseAndExit(params: HlpNativeCloseAndExitParams) {
    const r = await this.resolve(params);
    const exit = await this.exitInstruction(r, params.exit);
    const caller = address(params.exit.options.owner);
    const debtTokenProgram = await tokenProgramForMint(this.program.provider.connection, r.debt.assetMint);
    const callerBountyAccount = getAssociatedTokenAddressSync(r.debt.assetMint, caller, true, debtTokenProgram);
    const targetVault = r.target === 0 ? r.state.baseHlpVault : r.state.quoteHlpVault;
    const close = await this.write.instruction("closeInsolventHlp", {
      targetAsset: r.target,
      maxInsuranceDraw: governanceIntegerBN(params.maxInsuranceDraw ?? 0xffffffffffffffffn, "maxInsuranceDraw"),
      maxSocializedLoss: governanceIntegerBN(params.maxSocializedLoss ?? 0xffffffffffffffffn, "maxSocializedLoss"),
      minCallerBountyOut: governanceIntegerBN(params.minCallerBountyOut ?? 0, "minCallerBountyOut"),
    }, { accounts: {
      market: r.market, futarchyAuthority: r.futarchyAuthority, caller,
      borrowedMint: r.debt.assetMint, borrowedReserveVault: r.debt.reserveVault,
      borrowedInterestVault: r.debt.interestVault,
      insuranceVault: r.target === 0 ? r.state.insurance.quoteVault : r.state.insurance.baseVault,
      callerBountyAccount, ylpMint: r.state.ylpMint, targetHlpYlpVault: targetVault.ylpVault,
      tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID,
    }, remainingAccounts: params.closeRemainingAccounts ? [...params.closeRemainingAccounts] : undefined });
    const instructions = [
      createAssociatedTokenAccountIdempotentInstruction(caller, callerBountyAccount, caller, r.debt.assetMint, debtTokenProgram),
      ...exit.setupInstructions, close, exit.actionInstruction,
    ];
    return { closeInstruction: close, exitInstruction: exit.actionInstruction, instructions };
  }

  async buildFlash(params: HlpFlashParams) {
    if (typeof params.route !== "function") throw new Error("An explicit payment route is required");
    const r = await this.resolve(params), buyer = address(params.buyer);
    const [quote, collateralTokenProgram, debtTokenProgram] = await Promise.all([
      this.preview(params), tokenProgramForMint(this.program.provider.connection, r.collateral.assetMint),
      tokenProgramForMint(this.program.provider.connection, r.debt.assetMint),
    ]);
    if (quote.full && !params.allowFull) throw new Error("This vault needs full recovery; set allowFull explicitly");
    const exit = params.exit ? await this.exitInstruction(r, params.exit) : undefined;
    const [session] = PublicKey.findProgramAddressSync([Buffer.from("hlp-flash-session"), r.market.toBuffer(), Buffer.from([r.target])], this.program.programId);
    const [repaymentVault] = PublicKey.findProgramAddressSync([Buffer.from("hlp-flash-payment"), session.toBuffer()], this.program.programId);
    const buyerCollateralAccount = getAssociatedTokenAddressSync(r.collateral.assetMint, buyer, true, collateralTokenProgram);
    const buyerDebtAccount = getAssociatedTokenAddressSync(r.debt.assetMint, buyer, true, debtTokenProgram);
    const prefix = [...(params.prefixInstructions ?? []), ...(exit?.setupInstructions ?? [])];
    for (const [tag, instruction] of [[2, ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 })],
      [1, ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 })]] as const) {
      const matching = prefix.filter(ix => ix.programId.equals(ComputeBudgetProgram.programId) && ix.data[0] === tag);
      if (matching.length > 1) throw new Error("Duplicate compute budget instruction");
      if (!matching.length) prefix.unshift(instruction);
    }
    prefix.push(createAssociatedTokenAccountIdempotentInstruction(buyer, buyerCollateralAccount, buyer, r.collateral.assetMint, collateralTokenProgram),
      createAssociatedTokenAccountIdempotentInstruction(buyer, buyerDebtAccount, buyer, r.debt.assetMint, debtTokenProgram));
    const context: HlpFlashRouteContext = { quote, session, repaymentVault, buyerCollateralAccount, buyerDebtAccount,
      collateralMint: r.collateral.assetMint, debtMint: r.debt.assetMint, collateralTokenProgram, debtTokenProgram,
      requiredPaymentCredit: BigInt(quote.payment.toString()) };
    const route = [...await params.route(context)];
    if (route.some(ix => ix.programId.equals(ComputeBudgetProgram.programId))) throw new Error("Compute budgets must precede flash begin");
    const beginIndex = prefix.length, settleIndex = beginIndex + 1 + route.length;
    if (settleIndex > 65_535) throw new RangeError("Too many instructions for the bound settle index");
    const accounts = { market: r.market, futarchyAuthority: r.futarchyAuthority, buyer, targetMint: r.collateral.assetMint,
      borrowedMint: r.debt.assetMint, targetReserve: r.collateral.reserveVault, borrowedReserve: r.debt.reserveVault,
      buyerCollateralAccount, buyerRefundAccount: buyerDebtAccount,
      insuranceVault: r.target === 0 ? r.state.insurance.quoteVault : r.state.insurance.baseVault,
      tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID, borrowedTokenProgram: debtTokenProgram };
    const common = { accounts, session, repaymentVault, instructions: SYSVAR_INSTRUCTIONS_PUBKEY };
    const begin = await this.write.instruction("beginHlpFlash", {
      targetAsset: r.target, allowFull: params.allowFull ?? false,
      maxPayment: governanceIntegerBN(params.maxPayment ?? BigInt(quote.payment.toString()), "maxPayment"),
      maxCollateralDebit: governanceIntegerBN(params.maxCollateralDebit ?? BigInt(quote.collateralDebit.toString()), "maxCollateralDebit"),
      minCollateralCredit: governanceIntegerBN(params.minCollateralCredit ?? BigInt(quote.collateralCredit.toString()), "minCollateralCredit"),
      maxInsuranceDraw: governanceIntegerBN(params.maxInsuranceDraw ?? 0xffffffffffffffffn, "maxInsuranceDraw"),
      maxSocializedLoss: governanceIntegerBN(params.maxSocializedLoss ?? 0xffffffffffffffffn, "maxSocializedLoss"), settleIndex,
    }, { accounts: { ...common, systemProgram: SystemProgram.programId } });
    const settle = await this.write.instruction("settleHlpFlash", [], { accounts: common,
      remainingAccounts: params.settleRemainingAccounts ? [...params.settleRemainingAccounts] : await this.write.hlpRemainingAccounts(r.market, r.state) });
    return { ...context, beginIndex, settleIndex, beginInstruction: begin, settleInstruction: settle,
      exitInstruction: exit?.actionInstruction,
      instructions: [...prefix, begin, ...route, settle, ...(exit ? [exit.actionInstruction] : [])] };
  }
}
