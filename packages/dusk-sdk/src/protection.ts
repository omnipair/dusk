import type { IdlTypes } from "@coral-xyz/anchor";
import { PublicKey, type AccountMeta } from "@solana/web3.js";
import { TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID } from "@solana/spl-token";
import { SystemProgram } from "@solana/web3.js";
import type { LeverageDelegate } from "./types_delegate.js";
import { type LeverageDelegateProgram, LEVERAGE_DELEGATE_PROGRAM_ID } from "./delegate.js";
import { DUSK_PROGRAM_ID, deriveEventAuthorityAddress } from "./constants.js";
import { governanceIntegerBN } from "./governance.js";
import type { RawAmount } from "./write.js";

export const PROTECTION_ACTION = { borrowRepay: 0, borrowCollateral: 1, leverageRepay: 2 } as const;

export function deriveProtectionOrderAddress(owner: PublicKey, orderId: RawAmount,
  programId = LEVERAGE_DELEGATE_PROGRAM_ID): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from("protection_order"), owner.toBuffer(),
    governanceIntegerBN(orderId, "orderId").toArrayLike(Buffer, "le", 8)], programId);
}

type MethodAccounts<T extends keyof LeverageDelegateProgram["methods"]> =
  Parameters<ReturnType<LeverageDelegateProgram["methods"][T]>["accountsStrict"]>[0];
type CreateAccounts = Omit<MethodAccounts<"createProtectionOrder">,
  "order" | "duskEventAuthority" | "duskProgram" | "token2022Program" | "systemProgram"> & { owner: PublicKey };
type ManageAccounts = Omit<MethodAccounts<"fundProtectionOrder">, "token2022Program">;
type ExecuteAccounts = Omit<MethodAccounts<"executeProtectionOrder">,
  "duskEventAuthority" | "duskProgram" | "tokenProgram" | "token2022Program">;
type CreateArgs = IdlTypes<LeverageDelegate>["createProtectionOrderArgs"];

/** All amounts use raw token units. Health 10000 is the liquidation boundary. */
export type CreateProtectionOrderParams = {
  [K in keyof CreateArgs]: CreateArgs[K] extends number ? number : RawAmount;
};

/** Fund recurring protection for an existing borrowing or leverage position.
 * Setup the order's LP ATA, asset ATAs and both yield accounts first. LP transfers
 * require buildLpTransferHookAccountMetas. The sponsor remains yield recipient.
 * Execution needs a keeper token balance to advance repayment; LP redemption
 * reimburses that balance atomically. Insufficient redemption liquidity reverts.
 */
export class DuskProtectionOrders {
  constructor(readonly program: LeverageDelegateProgram) {}

  async createInstruction(params: CreateProtectionOrderParams, accounts: CreateAccounts,
    remainingAccounts: AccountMeta[] = []) {
    const order = deriveProtectionOrderAddress(new PublicKey(accounts.owner), params.orderId, this.program.programId)[0];
    const args: CreateArgs = {
      ...params,
      orderId: governanceIntegerBN(params.orderId, "orderId"),
      lpAmount: governanceIntegerBN(params.lpAmount, "lpAmount"),
      maxLpPerExecution: governanceIntegerBN(params.maxLpPerExecution, "maxLpPerExecution"),
      maxPaymentPerExecution: governanceIntegerBN(params.maxPaymentPerExecution, "maxPaymentPerExecution"),
      minPaymentPerLpNad: governanceIntegerBN(params.minPaymentPerLpNad, "minPaymentPerLpNad"),
      triggerHealthBps: governanceIntegerBN(params.triggerHealthBps, "triggerHealthBps"),
      targetHealthBps: governanceIntegerBN(params.targetHealthBps, "targetHealthBps"),
      expiresAt: governanceIntegerBN(params.expiresAt, "expiresAt"),
    };
    return this.program.methods.createProtectionOrder(args).accountsStrict({ ...accounts, order,
      duskProgram: DUSK_PROGRAM_ID, duskEventAuthority: deriveEventAuthorityAddress()[0],
      token2022Program: TOKEN_2022_PROGRAM_ID, systemProgram: SystemProgram.programId,
    }).remainingAccounts(remainingAccounts).instruction();
  }

  async previewInstruction(accounts: MethodAccounts<"previewProtectionOrder">) {
    return this.program.methods.previewProtectionOrder().accountsStrict(accounts).instruction();
  }

  async fundInstruction(lpAmount: RawAmount, accounts: ManageAccounts, remainingAccounts: AccountMeta[] = []) {
    return this.program.methods.fundProtectionOrder({ lpAmount: governanceIntegerBN(lpAmount, "lpAmount") })
      .accountsStrict({ ...accounts, token2022Program: TOKEN_2022_PROGRAM_ID }).remainingAccounts(remainingAccounts).instruction();
  }

  async cancelInstruction(accounts: ManageAccounts, remainingAccounts: AccountMeta[] = []) {
    return this.program.methods.cancelProtectionOrder().accountsStrict({ ...accounts, token2022Program: TOKEN_2022_PROGRAM_ID })
      .remainingAccounts(remainingAccounts).instruction();
  }

  /** LP redemptions may need an address lookup table and a larger heap frame.
   * Simulate the complete transaction with the current native market state.
   */
  async executeInstruction(params: { lpAmount: RawAmount; paymentAmount: RawAmount },
    accounts: ExecuteAccounts, remainingAccounts: AccountMeta[] = []) {
    return this.program.methods.executeProtectionOrder({ lpAmount: governanceIntegerBN(params.lpAmount, "lpAmount"),
      paymentAmount: governanceIntegerBN(params.paymentAmount, "paymentAmount") })
      .accountsStrict({ ...accounts, duskProgram: DUSK_PROGRAM_ID, duskEventAuthority: deriveEventAuthorityAddress()[0],
        tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID })
      .remainingAccounts(remainingAccounts).instruction();
  }
}
