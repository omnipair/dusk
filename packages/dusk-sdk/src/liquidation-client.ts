import type { Program, IdlTypes } from "@coral-xyz/anchor";
import { createAssociatedTokenAccountIdempotentInstruction, getAssociatedTokenAddressSync, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { ComputeBudgetProgram, PublicKey, SystemProgram, SYSVAR_INSTRUCTIONS_PUBKEY, type AccountMeta, type TransactionInstruction } from "@solana/web3.js";
import type { Dusk as DuskIdl } from "./types_v2.js";
import { address, type AddressLike } from "./address.js";
import { DuskWrite } from "./write.js";
import { DuskGet } from "./get.js";
import { previewReturnDataBytes } from "./preview.js";
import { governanceIntegerBN, type GovernanceIntegerLike } from "./governance.js";
import { tokenProgramForMint } from "./referral.js";
import { deriveFutarchyAuthorityAddress, deriveLeverageCollateralVaultAddress, deriveReferralAccrualAddress } from "./constants.js";
import { deriveLiquidationPaymentAddress, deriveLiquidationSessionAddress } from "./liquidation.js";

export type FlashLiquidationQuote = IdlTypes<DuskIdl>["flashLiquidationQuote"];
export type EmergencyLiquidationQuote = IdlTypes<DuskIdl>["emergencyLiquidationQuote"];

export interface LiquidationPositionParams {
  market: AddressLike;
  position: AddressLike;
  kind: "borrow" | "leverage";
  debtAsset: "base" | "quote";
}

export interface FlashLiquidationParams extends LiquidationPositionParams {
  buyer: AddressLike;
  maxRepayment: GovernanceIntegerLike;
  full?: boolean;
  /** Net escrow balance increase, including all outgoing transfer gross-ups. */
  maxPayment?: GovernanceIntegerLike;
  /** Actual buyer receipt after collateral transfer fees. */
  minCollateralCredit?: GovernanceIntegerLike;
  prefixInstructions?: readonly TransactionInstruction[];
  /** Dusk execution sells the received collateral in the paired settle and
   * nets debt principal internally. External settlement requires route payment. */
  settlement?: "external" | "duskAmm";
  /** Override when the route changes which hLP accounts settlement requires.
   * Supply the complete ordered remaining-account list for the final state. */
  settleRemainingAccounts?: readonly AccountMeta[];
  /** Builds the collateral sale, inventory payment, or mixed route. The
   * resulting instructions run after begin and before the exact paired settle.
   */
  route?: (context: FlashLiquidationRouteContext) => Promise<readonly TransactionInstruction[]> | readonly TransactionInstruction[];
}

export interface FlashLiquidationRouteContext {
  session: PublicKey;
  repaymentVault: PublicKey;
  buyerCollateralAccount: PublicKey;
  buyerDebtAccount: PublicKey;
  collateralMint: PublicKey;
  debtMint: PublicKey;
  debtTokenProgram: PublicKey;
  collateralTokenProgram: PublicKey;
  /** Route must deliver at least this net spendable amount into repaymentVault. */
  requiredPaymentCredit: bigint;
  quote: FlashLiquidationQuote;
}

export interface EmergencyLiquidationParams extends LiquidationPositionParams {
  caller: AddressLike;
  collateralDebit: GovernanceIntegerLike;
  full?: boolean;
  minRewardCredit?: GovernanceIntegerLike;
  minSwapOutput?: GovernanceIntegerLike;
  prefixInstructions?: readonly TransactionInstruction[];
}

function liquidationPrefix(instructions: readonly TransactionInstruction[] = []): TransactionInstruction[] {
  const prefix = [...instructions];
  if (!prefix.some(ix => ix.programId.equals(ComputeBudgetProgram.programId) && ix.data[0] === 1)) {
    prefix.unshift(ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }));
  }
  if (!prefix.some(ix => ix.programId.equals(ComputeBudgetProgram.programId) && ix.data[0] === 2)) {
    prefix.unshift(ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }));
  }
  return prefix;
}

/** Fixed-payment liquidation helpers. No method signs or sends a transaction,
 * debits the owner's wallet, or chooses an external routing program.
 */
export class DuskLiquidations {
  readonly write: DuskWrite;
  readonly get: DuskGet;

  constructor(readonly program: Program<DuskIdl>, feePayer?: AddressLike) {
    this.write = new DuskWrite(program);
    this.get = new DuskGet(program, feePayer);
  }

  private async resolve(params: LiquidationPositionParams) {
    if (params.kind !== "borrow" && params.kind !== "leverage") throw new Error("Invalid position kind");
    if (params.debtAsset !== "base" && params.debtAsset !== "quote") throw new Error("Invalid debt asset");
    const market = address(params.market);
    const position = address(params.position);
    const [state, positionState] = await Promise.all([
      this.program.account.market.fetch(market),
      params.kind === "borrow" ? this.program.account.borrowPosition.fetch(position) : this.program.account.leveragePosition.fetch(position),
    ]);
    if (!positionState.market.equals(market)) throw new Error("Position belongs to another market");
    const debtAsset = params.debtAsset === "base" ? 0 : 1;
    if ("debtAsset" in positionState && positionState.debtAsset !== debtAsset) throw new Error("Wrong leverage debt side");
    const debtSide = debtAsset === 0 ? state.baseSide : state.quoteSide;
    const collateralSide = debtAsset === 0 ? state.quoteSide : state.baseSide;
    const readAccounts = { market, borrowPosition: params.kind === "borrow" ? position : null, leveragePosition: params.kind === "leverage" ? position : null, debtMint: debtSide.assetMint, collateralMint: collateralSide.assetMint };
    return { market, position, state, positionState, debtSide, collateralSide, debtAsset, readAccounts };
  }

  /** Commit separately if a failed execution route must not erase the timer. */
  async observeInstruction(params: LiquidationPositionParams): Promise<TransactionInstruction> {
    const resolved = await this.resolve(params);
    return this.write.instruction("observeLiquidation", [resolved.debtAsset], { accounts: resolved.readAccounts });
  }

  async preview(params: LiquidationPositionParams & { maxRepayment: GovernanceIntegerLike; full?: boolean }): Promise<FlashLiquidationQuote> {
    const resolved = await this.resolve(params);
    const instruction = await this.write.instruction("previewFlashLiquidation", {
      debtAsset: resolved.debtAsset, maxRepayment: governanceIntegerBN(params.maxRepayment, "maxRepayment"), full: params.full ?? false,
    }, { accounts: resolved.readAccounts });
    const returned = await this.get.simulateReturnData(instruction);
    return this.program.coder.types.decode("flashLiquidationQuote", previewReturnDataBytes(returned)) as FlashLiquidationQuote;
  }

  async previewEmergency(params: LiquidationPositionParams & { collateralDebit: GovernanceIntegerLike; full?: boolean }): Promise<EmergencyLiquidationQuote> {
    const r = await this.resolve(params);
    const instruction = await this.write.instruction("previewEmergencyLiquidation", {
      debtAsset: r.debtAsset, collateralDebit: governanceIntegerBN(params.collateralDebit, "collateralDebit"), full: params.full ?? false,
    }, { accounts: { preview: r.readAccounts, futarchyAuthority: deriveFutarchyAuthorityAddress(this.program.programId)[0] } });
    const returned = await this.get.simulateReturnData(instruction);
    return this.program.coder.types.decode("emergencyLiquidationQuote", previewReturnDataBytes(returned)) as EmergencyLiquidationQuote;
  }

  /** Direct partial-first AMM liquidation. The program nets principal internally
   * and verifies the emergency/full-close gates; this helper cannot bypass them.
   */
  async buildEmergency(params: EmergencyLiquidationParams) {
    const r = await this.resolve(params);
    const buyer = address(params.caller);
    const [debtTokenProgram, collateralTokenProgram, quote, remainingAccounts] = await Promise.all([
      tokenProgramForMint(this.program.provider.connection, r.debtSide.assetMint),
      tokenProgramForMint(this.program.provider.connection, r.collateralSide.assetMint),
      this.previewEmergency(params), this.write.hlpRemainingAccounts(r.market, r.state),
    ]);
    const buyerCollateralAccount = getAssociatedTokenAddressSync(r.collateralSide.assetMint, buyer, true, collateralTokenProgram);
    const buyerRefundAccount = getAssociatedTokenAddressSync(r.debtSide.assetMint, buyer, true, debtTokenProgram);
    const ownerDebtAccount = getAssociatedTokenAddressSync(r.debtSide.assetMint, r.positionState.owner, true, debtTokenProgram);
    const prefix = liquidationPrefix(params.prefixInstructions);
    const seen = new Set<string>();
    for (const [account, mint, owner, token] of [
      [buyerCollateralAccount, r.collateralSide.assetMint, buyer, collateralTokenProgram],
      [buyerRefundAccount, r.debtSide.assetMint, buyer, debtTokenProgram],
      [ownerDebtAccount, r.debtSide.assetMint, r.positionState.owner, debtTokenProgram],
    ]) {
      if (!seen.has(account.toBase58())) prefix.push(createAssociatedTokenAccountIdempotentInstruction(buyer, account, owner, mint, token));
      seen.add(account.toBase58());
    }
    const p = r.positionState;
    const partner = "referralPartner" in p ? p.referralPartner : r.debtAsset === 0 ? p.baseReferralPartner : p.quoteReferralPartner;
    const referralPartner = partner.equals(PublicKey.default) ? null : partner;
    const accounts = { ...r.readAccounts, buyer, buyerCollateralAccount, buyerRefundAccount, ownerDebtAccount,
      collateralVault: params.kind === "borrow" ? r.collateralSide.collateralVault : deriveLeverageCollateralVaultAddress(r.market, r.collateralSide.assetMint, this.program.programId)[0],
      reserveVault: r.debtSide.reserveVault, interestVault: r.debtSide.interestVault,
      insuranceVault: r.debtAsset === 0 ? r.state.insurance.baseVault : r.state.insurance.quoteVault,
      futarchyAuthority: deriveFutarchyAuthorityAddress(this.program.programId)[0], referralPartner,
      referralAccrual: referralPartner ? deriveReferralAccrualAddress(referralPartner, r.market, r.debtSide.assetMint, this.program.programId)[0] : null,
      tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID, debtTokenProgram };
    const instruction = await this.write.instruction("emergencyLiquidation", {
      debtAsset: r.debtAsset, collateralDebit: governanceIntegerBN(params.collateralDebit, "collateralDebit"), full: params.full ?? false,
      minRewardCredit: governanceIntegerBN(params.minRewardCredit ?? BigInt(quote.rewardCredit.toString()), "minRewardCredit"),
      minSwapOutput: governanceIntegerBN(params.minSwapOutput ?? BigInt(quote.swapOutput.toString()), "minSwapOutput"),
    }, { accounts: { accounts, collateralReserveVault: r.collateralSide.reserveVault, instructions: SYSVAR_INSTRUCTIONS_PUBKEY }, remainingAccounts });
    return { quote, instruction, instructions: [...prefix, instruction], callerDebtAccount: buyerRefundAccount, ownerDebtAccount };
  }

  /** Prefix + begin + caller's route + settle, with indices computed last.
   * Do not subsequently prepend/insert instructions; build another plan.
   * The persistent payment account retains withheld Token-2022 fees, avoiding
   * an invalid close-account CPI when the mint charges transfer fees.
   */
  async buildFlash(params: FlashLiquidationParams) {
    const internal = params.settlement === "duskAmm";
    if (params.settlement !== undefined && params.settlement !== "external" && !internal) throw new Error("Invalid flash settlement venue");
    if (!internal && !params.route) throw new Error("External settlement requires an explicit payment route");
    const r = await this.resolve(params);
    const buyer = address(params.buyer);
    const [debtTokenProgram, collateralTokenProgram, quote] = await Promise.all([
      tokenProgramForMint(this.program.provider.connection, r.debtSide.assetMint),
      tokenProgramForMint(this.program.provider.connection, r.collateralSide.assetMint),
      this.preview(params),
    ]);
    const [session] = deriveLiquidationSessionAddress(r.position, this.program.programId);
    const [repaymentVault] = deriveLiquidationPaymentAddress(r.position, r.debtSide.assetMint, this.program.programId);
    const buyerCollateralAccount = getAssociatedTokenAddressSync(r.collateralSide.assetMint, buyer, true, collateralTokenProgram);
    const buyerDebtAccount = getAssociatedTokenAddressSync(r.debtSide.assetMint, buyer, true, debtTokenProgram);
    const ownerDebtAccount = getAssociatedTokenAddressSync(r.debtSide.assetMint, r.positionState.owner, true, debtTokenProgram);
    const prefix = liquidationPrefix(params.prefixInstructions);
    const seen = new Set<string>();
    for (const [account, mint, owner, token] of [
      [buyerCollateralAccount, r.collateralSide.assetMint, buyer, collateralTokenProgram],
      [buyerDebtAccount, r.debtSide.assetMint, buyer, debtTokenProgram],
      [ownerDebtAccount, r.debtSide.assetMint, r.positionState.owner, debtTokenProgram],
    ]) {
      if (!seen.has(account.toBase58())) prefix.push(createAssociatedTokenAccountIdempotentInstruction(buyer, account, owner, mint, token));
      seen.add(account.toBase58());
    }
    const context: FlashLiquidationRouteContext = {
      session, repaymentVault, buyerCollateralAccount, buyerDebtAccount, debtTokenProgram, collateralTokenProgram,
      collateralMint: r.collateralSide.assetMint, debtMint: r.debtSide.assetMint,
      requiredPaymentCredit: internal ? 0n : BigInt(quote.payment.toString()), quote,
    };
    const route = params.route ? [...await params.route(context)] : [];
    const beginIndex = prefix.length;
    const settleIndex = beginIndex + 1 + route.length;
    if (settleIndex > 65_535) throw new RangeError("Too many instructions for the bound settle index");
    const p = r.positionState;
    const partner = "referralPartner" in p ? p.referralPartner : r.debtAsset === 0 ? p.baseReferralPartner : p.quoteReferralPartner;
    const referralPartner = partner.equals(PublicKey.default) ? null : partner;
    const accounts = {
      ...r.readAccounts, buyer, buyerCollateralAccount, buyerRefundAccount: buyerDebtAccount, ownerDebtAccount,
      collateralVault: params.kind === "borrow" ? r.collateralSide.collateralVault : deriveLeverageCollateralVaultAddress(r.market, r.collateralSide.assetMint, this.program.programId)[0],
      reserveVault: r.debtSide.reserveVault, interestVault: r.debtSide.interestVault,
      insuranceVault: r.debtAsset === 0 ? r.state.insurance.baseVault : r.state.insurance.quoteVault,
      futarchyAuthority: deriveFutarchyAuthorityAddress(this.program.programId)[0], referralPartner,
      referralAccrual: referralPartner ? deriveReferralAccrualAddress(referralPartner, r.market, r.debtSide.assetMint, this.program.programId)[0] : null,
      tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID, debtTokenProgram,
    };
    const common = { accounts, session, repaymentVault, instructions: SYSVAR_INSTRUCTIONS_PUBKEY };
    const begin = await this.write.instruction("beginFlashLiquidation", {
      position: r.position, debtAsset: r.debtAsset, maxRepayment: governanceIntegerBN(params.maxRepayment, "maxRepayment"),
      maxPayment: governanceIntegerBN(params.maxPayment ?? BigInt(quote.payment.toString()), "maxPayment"),
      minCollateralCredit: governanceIntegerBN(params.minCollateralCredit ?? BigInt(quote.collateralCredit.toString()), "minCollateralCredit"),
      full: params.full ?? false, settleIndex,
    }, { accounts: { ...common, systemProgram: SystemProgram.programId } });
    const settle = await this.write.instruction("settleFlashLiquidation", [], {
      accounts: { ...common, collateralReserveVault: internal ? r.collateralSide.reserveVault : null },
      remainingAccounts: params.settleRemainingAccounts ? [...params.settleRemainingAccounts]
        : internal ? await this.write.hlpRemainingAccounts(r.market, r.state) : [],
    });
    return { ...context, beginIndex, settleIndex, instructions: [...prefix, begin, ...route, settle], beginInstruction: begin, settleInstruction: settle };
  }
}
