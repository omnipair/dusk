import type { Program } from "@coral-xyz/anchor";
import { createAssociatedTokenAccountIdempotentInstruction, getAssociatedTokenAddressSync, TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID } from "@solana/spl-token";
import { ComputeBudgetProgram, PublicKey, SystemProgram, SYSVAR_INSTRUCTIONS_PUBKEY, type AccountMeta, type TransactionInstruction } from "@solana/web3.js";
import { address, type AddressLike } from "./address.js";
import { deriveFutarchyAuthorityAddress } from "./constants.js";
import { DuskGet } from "./get.js";
import { integerBN, type IntegerLike } from "./parameters.js";
import { previewReturnDataBytes } from "./preview.js";
import { tokenProgramForMint } from "./referral.js";
import type { Dusk as DuskIdl } from "./types_v2.js";
import type { HlpFlashQuote } from "./type-aliases.js";
import { DuskWrite, type HlpLiquidityBuildOptions } from "./write.js";

export type { HlpFlashQuote };
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
interface HlpFlashBaseParams extends HlpRecoveryParams {
  buyer: AddressLike;
  /** Explicit opt-in to closing an insolvent vault's entire backing. */
  allowFull?: boolean;
  maxPayment?: IntegerLike;
  maxCollateralDebit?: IntegerLike;
  minCollateralCredit?: IntegerLike;
  prefixInstructions?: readonly TransactionInstruction[];
  settleRemainingAccounts?: readonly AccountMeta[];
  route: (context: HlpFlashRouteContext) => Promise<readonly TransactionInstruction[]> | readonly TransactionInstruction[];
  /** Append an owner-signed withdrawal after funded recovery or terminal
   * settlement. A terminal close pays zero principal. */
  exit?: HlpRecoveryExit;
}

/**
 * Bounds the principal a full (terminal) settlement writes off; a partial
 * rescue writes off none. There is no default. Pass `maxSocializedLoss`, or
 * pass the reviewed `quote` from `preview` to cap the loss at that quote's
 * write-off: zero for a partial quote, otherwise the vault's debt principal
 * (read when the plan is built) less the quote's recovery,
 * `borrowedClaim + reserveCredit`. A passed `quote` also replaces the
 * builder's own preview, so the payment and collateral bounds and the route
 * context come from it.
 */
export type HlpFlashLossBound =
  | { maxSocializedLoss: IntegerLike; quote?: HlpFlashQuote }
  | { maxSocializedLoss?: undefined; quote: HlpFlashQuote };

export type HlpFlashParams = HlpFlashBaseParams & HlpFlashLossBound;

export interface HlpRecoveryExit {
  hlpAmount: IntegerLike;
  /** Floor on the target asset the holder receives. Required: positive after a
   * solvent (partial) rescue; zero only after a full flash or native terminal
   * close, whose receipt burn pays no principal. */
  minTargetAmountOut: IntegerLike;
  options: HlpLiquidityBuildOptions<"withdrawSingleSided">;
}

export interface HlpNativeCloseAndExitParams extends HlpRecoveryParams {
  exit: HlpRecoveryExit;
  /** Required cap on the principal the native close writes off. */
  maxSocializedLoss: IntegerLike;
  minCallerBountyOut?: IntegerLike;
  closeRemainingAccounts?: readonly AccountMeta[];
}

/** Atomic vault recovery. A solvent funding gap can be rescued before the
 * native critical-stress boundary; a full quote still requires insolvency.
 * Keeps a fixed payment obligation and permits an external route or inventory
 * payment. A direct Dusk swap may be used when the complete transaction fits
 * Solana's compute limit; callers must simulate their chosen route. Native
 * Dusk recovery remains available through an ordinary swap. Never signs or
 * submits. */
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
    const state = await this.get.market(market);
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

  /** `terminal` exits follow a close that pays no principal; any other exit
   * needs a positive payout floor. */
  private async exitInstruction(r: Awaited<ReturnType<DuskHlpLiquidations["resolve"]>>, exit: HlpRecoveryExit, terminal: boolean) {
    if (!address(exit.options.market).equals(r.market)) throw new Error("Exit market differs from liquidation market");
    if (!address(exit.options.targetHlpMint).equals(r.collateral.hlpMint)) throw new Error("Exit hLP mint differs from liquidation target");
    if (!address(exit.options.baseMint).equals(r.state.baseSide.assetMint) ||
        !address(exit.options.quoteMint).equals(r.state.quoteSide.assetMint)) throw new Error("Exit asset mints differ from market");
    if (exit.minTargetAmountOut === undefined) throw new Error("exit.minTargetAmountOut is required");
    const minTargetAmountOut = integerBN(exit.minTargetAmountOut, "minTargetAmountOut");
    if (!terminal && minTargetAmountOut.isZero()) {
      throw new Error("A solvent rescue's exit pays the holder; set a positive exit.minTargetAmountOut");
    }
    const built = await this.write.withdrawSingleSided({
      hlpAmount: integerBN(exit.hlpAmount, "hlpAmount").toString(),
      minTargetAmountOut: minTargetAmountOut.toString(),
    }, exit.options);
    return built;
  }

  /** One signed transaction: permissionless native junior/senior loss close,
   * then the holder burns receipts. Eligibility uses current-slot symmetric
   * EMA health; native recovery uses EMA price, smoothed depth, concentration,
   * and the configured fee schedule. The receipt burn pays zero principal.
   * `maxSocializedLoss` is required. */
  async buildNativeCloseAndExit(params: HlpNativeCloseAndExitParams) {
    if (params.maxSocializedLoss === undefined) throw new Error("maxSocializedLoss is required");
    const maxSocializedLoss = integerBN(params.maxSocializedLoss, "maxSocializedLoss");
    const r = await this.resolve(params);
    const exit = await this.exitInstruction(r, params.exit, true);
    const caller = address(params.exit.options.owner);
    const debtTokenProgram = await tokenProgramForMint(this.program.provider.connection, r.debt.assetMint);
    const callerBountyAccount = getAssociatedTokenAddressSync(r.debt.assetMint, caller, true, debtTokenProgram);
    const targetVault = r.target === 0 ? r.state.baseHlpVault : r.state.quoteHlpVault;
    const close = await this.write.instruction("closeInsolventHlp", {
      targetAsset: r.target,
      maxSocializedLoss,
      minCallerBountyOut: integerBN(params.minCallerBountyOut ?? 0, "minCallerBountyOut"),
    }, { accounts: {
      market: r.market, futarchyAuthority: r.futarchyAuthority, caller,
      borrowedMint: r.debt.assetMint, borrowedReserveVault: r.debt.reserveVault,
      borrowedInterestVault: r.debt.interestVault,
      callerBountyAccount, srlpMint: r.state.srlpMint, targetHlpSrlpVault: targetVault.srlpVault,
      tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID,
    }, remainingAccounts: params.closeRemainingAccounts ? [...params.closeRemainingAccounts] : undefined });
    const instructions = [
      createAssociatedTokenAccountIdempotentInstruction(caller, callerBountyAccount, caller, r.debt.assetMint, debtTokenProgram),
      ...exit.setupInstructions, close, exit.actionInstruction,
    ];
    return { closeInstruction: close, exitInstruction: exit.actionInstruction, instructions };
  }

  /** `maxSocializedLoss` or a reviewed `quote` is required; see `HlpFlashLossBound`. */
  async buildFlash(params: HlpFlashParams) {
    if (typeof params.route !== "function") throw new Error("An explicit payment route is required");
    if (params.maxSocializedLoss === undefined && !params.quote) {
      throw new Error("maxSocializedLoss is required; pass it or the reviewed preview quote");
    }
    const r = await this.resolve(params), buyer = address(params.buyer);
    const [quote, collateralTokenProgram, debtTokenProgram] = await Promise.all([
      params.quote ?? this.preview(params), tokenProgramForMint(this.program.provider.connection, r.collateral.assetMint),
      tokenProgramForMint(this.program.provider.connection, r.debt.assetMint),
    ]);
    if (params.quote && params.quote.targetAsset !== r.target) throw new Error("Quote is for the other hLP vault");
    if (quote.full && !params.allowFull) throw new Error("This vault needs full recovery; set allowFull explicitly");
    const maxSocializedLoss = params.maxSocializedLoss !== undefined
      ? integerBN(params.maxSocializedLoss, "maxSocializedLoss")
      : integerBN(quotedPrincipalLoss(quote, r.target === 0 ? r.state.baseHlpVault : r.state.quoteHlpVault), "maxSocializedLoss");
    const exit = params.exit ? await this.exitInstruction(r, params.exit, quote.full) : undefined;
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
      tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID, borrowedTokenProgram: debtTokenProgram };
    const common = { accounts, session, repaymentVault, instructions: SYSVAR_INSTRUCTIONS_PUBKEY };
    const begin = await this.write.instruction("beginHlpFlash", {
      targetAsset: r.target, allowFull: params.allowFull ?? false,
      maxPayment: integerBN(params.maxPayment ?? BigInt(quote.payment.toString()), "maxPayment"),
      maxCollateralDebit: integerBN(params.maxCollateralDebit ?? BigInt(quote.collateralDebit.toString()), "maxCollateralDebit"),
      minCollateralCredit: integerBN(params.minCollateralCredit ?? BigInt(quote.collateralCredit.toString()), "minCollateralCredit"),
      maxSocializedLoss, settleIndex,
    }, { accounts: { ...common, systemProgram: SystemProgram.programId } });
    const settle = await this.write.instruction("settleHlpFlash", [], { accounts: common,
      remainingAccounts: params.settleRemainingAccounts ? [...params.settleRemainingAccounts] : await this.write.hlpRemainingAccounts(r.market, r.state) });
    return { ...context, beginIndex, settleIndex, beginInstruction: begin, settleInstruction: settle,
      exitInstruction: exit?.actionInstruction,
      instructions: [...prefix, begin, ...route, settle, ...(exit ? [exit.actionInstruction] : [])] };
  }
}

/** Principal a full settlement of `quote` writes off. The program measures the
 * recovery against min(debt principal, debt), so the debt principal alone is
 * the larger bound. A partial rescue writes off none. */
function quotedPrincipalLoss(quote: HlpFlashQuote, vault: { debtPrincipal: { toString(): string } }): bigint {
  if (!quote.full) return 0n;
  const recovery = BigInt(quote.borrowedClaim.toString()) + BigInt(quote.reserveCredit.toString());
  const principal = BigInt(vault.debtPrincipal.toString());
  return principal > recovery ? principal - recovery : 0n;
}
