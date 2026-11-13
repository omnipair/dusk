import type { IdlTypes } from "@coral-xyz/anchor";
import { PublicKey, type AccountMeta } from "@solana/web3.js";
import { TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID } from "@solana/spl-token";
import { SystemProgram } from "@solana/web3.js";
import type { LeverageDelegate } from "./types_delegate.js";
import { type LeverageDelegateProgram, LEVERAGE_DELEGATE_PROGRAM_ID } from "./delegate.js";
import { DUSK_PROGRAM_ID, deriveEventAuthorityAddress } from "./constants.js";
import {
  normalizeInstructionAccounts,
  normalizeInstructionArgs,
  type LeverageDelegateInstructionName,
} from "./instruction-args.js";
import { integerBN } from "./parameters.js";
import { DuskWrite, type RawAmount } from "./write.js";
import { createDuskProgram } from "./program.js";
import { oracleReadAccounts } from "./oracle.js";
import { assertCurrentMarketLayout } from "./market-layout.js";

export const PROTECTION_ACTION = { borrowRepay: 0, borrowCollateral: 1, leverageRepay: 2 } as const;

export function deriveProtectionOrderAddress(owner: PublicKey, orderId: RawAmount,
  programId = LEVERAGE_DELEGATE_PROGRAM_ID): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from("protection_order"), owner.toBuffer(),
    integerBN(orderId, "orderId").toArrayLike(Buffer, "le", 8)], programId);
}

type MethodAccounts<T extends keyof LeverageDelegateProgram["methods"]> =
  Parameters<ReturnType<LeverageDelegateProgram["methods"][T]>["accountsStrict"]>[0];
type CreateAccounts = Omit<MethodAccounts<"createProtectionOrder">,
  "order" | "duskEventAuthority" | "duskProgram" | "token2022Program" | "systemProgram"> & { owner: PublicKey };
type ManageAccounts = Omit<MethodAccounts<"fundProtectionOrder">, "token2022Program">;
type CloseAccounts = Omit<MethodAccounts<"closeProtectionOrder">, "tokenProgram" | "token2022Program">;
type ExecuteAccounts = Omit<MethodAccounts<"executeProtectionOrder">,
  "duskEventAuthority" | "duskProgram" | "tokenProgram" | "token2022Program">;
type CreateArgs = IdlTypes<LeverageDelegate>["createProtectionOrderArgs"];
type FundArgs = IdlTypes<LeverageDelegate>["fundProtectionOrderArgs"];
type ExecuteArgs = IdlTypes<LeverageDelegate>["executeProtectionOrderArgs"];

/** All amounts use raw token units. Health 10000 is the liquidation boundary. */
export type CreateProtectionOrderParams = {
  [K in keyof CreateArgs]: CreateArgs[K] extends number ? number : RawAmount;
};

/** Fund recurring protection for an existing borrowing or leverage position.
 * Setup the order's LP ATA, asset ATAs and both yield accounts first. LP transfers
 * require buildLpTransferHookAccountMetas. The sponsor remains yield recipient.
 * Execution needs a keeper token balance to advance repayment; LP redemption
 * reimburses that balance atomically. Insufficient redemption liquidity reverts.
 * Cancellation refunds the LP; after harvesting the order's yield, closing it
 * returns the rent of the order and its custody accounts.
 */
export class DuskProtectionOrders {
  constructor(readonly program: LeverageDelegateProgram) {}

  // The same checks as the generic builders: a missing, misspelled or wrongly
  // typed field or an undeclared account throws `DuskInstructionInputError`
  // before anything is fetched or encoded.
  private checkedArgs<T>(name: LeverageDelegateInstructionName, args: unknown): T {
    return normalizeInstructionArgs(this.program.idl, name, args)[0] as T;
  }

  private checkedAccounts<T>(name: LeverageDelegateInstructionName, accounts: object): T {
    return normalizeInstructionAccounts(this.program.idl, name, accounts) as T;
  }

  private async oracleAccounts(market: PublicKey, remaining: AccountMeta[]) {
    const dusk = createDuskProgram({ connection: this.program.provider.connection });
    const state = assertCurrentMarketLayout(await dusk.account.market.fetch(market));
    const accounts = [...remaining];
    for (const meta of oracleReadAccounts(state.oracle.policy)) {
      if (!accounts.some(account => account.pubkey.equals(meta.pubkey))) accounts.push(meta);
    }
    return accounts;
  }

  async createInstruction(params: CreateProtectionOrderParams, accounts: CreateAccounts,
    remainingAccounts: AccountMeta[] = []) {
    const args = this.checkedArgs<CreateArgs>("createProtectionOrder", params);
    const callerAccounts = this.checkedAccounts<CreateAccounts>("createProtectionOrder", accounts);
    const order = deriveProtectionOrderAddress(callerAccounts.owner, args.orderId.toString(), this.program.programId)[0];
    return this.program.methods.createProtectionOrder(args).accountsStrict({ ...callerAccounts, order,
      duskProgram: DUSK_PROGRAM_ID, duskEventAuthority: deriveEventAuthorityAddress()[0],
      token2022Program: TOKEN_2022_PROGRAM_ID, systemProgram: SystemProgram.programId,
    }).remainingAccounts(remainingAccounts).instruction();
  }

  async previewInstruction(accounts: MethodAccounts<"previewProtectionOrder">, remainingAccounts: AccountMeta[] = []) {
    const checked = this.checkedAccounts<MethodAccounts<"previewProtectionOrder">>("previewProtectionOrder", accounts);
    return this.program.methods.previewProtectionOrder().accountsStrict(checked)
      .remainingAccounts(await this.oracleAccounts(new PublicKey(checked.market), remainingAccounts)).instruction();
  }

  async fundInstruction(lpAmount: RawAmount, accounts: ManageAccounts, remainingAccounts: AccountMeta[] = []) {
    const args = this.checkedArgs<FundArgs>("fundProtectionOrder", { lpAmount });
    const checked = this.checkedAccounts<MethodAccounts<"fundProtectionOrder">>("fundProtectionOrder",
      { ...accounts, token2022Program: TOKEN_2022_PROGRAM_ID });
    return this.program.methods.fundProtectionOrder(args).accountsStrict(checked).remainingAccounts(remainingAccounts).instruction();
  }

  async cancelInstruction(accounts: ManageAccounts, remainingAccounts: AccountMeta[] = []) {
    const checked = this.checkedAccounts<MethodAccounts<"cancelProtectionOrder">>("cancelProtectionOrder",
      { ...accounts, token2022Program: TOKEN_2022_PROGRAM_ID });
    return this.program.methods.cancelProtectionOrder().accountsStrict(checked)
      .remainingAccounts(remainingAccounts).instruction();
  }

  /** Close a cancelled order and return its rent to the sponsor. The LP custody
   * must be empty and both yield accounts harvested. Pass the order's asset
   * custody accounts to close them too, or null; a custody account holding a
   * balance needs the sponsor's token account for that mint, which receives it.
   */
  async closeInstruction(accounts: CloseAccounts, remainingAccounts: AccountMeta[] = []) {
    const checked = this.checkedAccounts<MethodAccounts<"closeProtectionOrder">>("closeProtectionOrder",
      { ...accounts, tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID });
    return this.program.methods.closeProtectionOrder().accountsStrict(checked)
      .remainingAccounts(remainingAccounts).instruction();
  }

  /** LP redemptions may need an address lookup table and a larger heap frame.
   * Simulate the complete transaction with the current native market state.
   * LP proceeds must cover payment, the full keeper reward, and the additional
   * 10 bps protocol fee. Supply protocolFee from orderProtocolFeeAccounts.
   */
  async executeInstruction(params: { lpAmount: RawAmount; paymentAmount: RawAmount },
    accounts: ExecuteAccounts, remainingAccounts: AccountMeta[] = []) {
    const args = this.checkedArgs<ExecuteArgs>("executeProtectionOrder", params);
    const checked = this.checkedAccounts<MethodAccounts<"executeProtectionOrder">>("executeProtectionOrder", {
      ...accounts, duskProgram: DUSK_PROGRAM_ID, duskEventAuthority: deriveEventAuthorityAddress()[0],
      tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID,
    });
    const write = new DuskWrite(createDuskProgram({ connection: this.program.provider.connection }));
    const order = await this.program.account.protectionOrder.fetch(checked.order);
    const prefix = order.action === PROTECTION_ACTION.borrowCollateral
      ? [] : await write.hlpRemainingAccounts(new PublicKey(checked.market));
    // Keep the canonical prefix first even when callers supply transfer-hook
    // accounts. Existing callers may already provide that same prefix.
    const hasPrefix = prefix.length > 0 && prefix.every((meta, i) =>
      remainingAccounts[i]?.pubkey.equals(meta.pubkey) && remainingAccounts[i]?.isWritable === meta.isWritable);
    const completeRemaining = hasPrefix ? remainingAccounts : [...prefix, ...remainingAccounts];
    const remaining = await this.oracleAccounts(new PublicKey(checked.market), completeRemaining);
    return this.program.methods.executeProtectionOrder(args).accountsStrict(checked)
      .remainingAccounts(remaining).instruction();
  }
}
