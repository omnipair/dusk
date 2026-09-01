import { AnchorProvider, Program } from "@coral-xyz/anchor";
import {
  PublicKey,
  SystemProgram,
  Transaction,
  type AccountMeta,
  type TransactionInstruction,
} from "@solana/web3.js";

import { address, type AddressLike } from "./address.js";
import { deriveLeveragePositionAddress } from "./constants.js";
import { toBN } from "./governance.js";
import DELEGATE_IDL from "./idl_delegate.js";
import type { LeverageDelegate } from "./types_delegate.js";

export const LEVERAGE_DELEGATE_PROGRAM_ID = address(
  "AXNfmZt5e1UM4daeTzW3H7zNo4boobBcnFm8RzJYxvAv"
);

export type LeverageDelegateProgram = Program<LeverageDelegate>;

export function leverageDelegateIdl(
  programId: AddressLike = LEVERAGE_DELEGATE_PROGRAM_ID
): LeverageDelegate {
  const idl = JSON.parse(JSON.stringify(DELEGATE_IDL)) as LeverageDelegate;
  idl.address = address(programId).toBase58() as LeverageDelegate["address"];
  return idl;
}

export function createLeverageDelegateProgram(options: {
  provider: AnchorProvider;
  programId?: AddressLike;
}): LeverageDelegateProgram {
  return new Program<LeverageDelegate>(
    leverageDelegateIdl(options.programId),
    options.provider
  );
}

/**
 * Conditional order kinds.
 *
 * The program stores `kind` as an opaque u8 and does not define an enum for
 * it, so the meaning is a caller convention. These are the values the protocol
 * already uses; changing them silently reinterprets every existing order.
 */
export const LEVERAGE_ORDER_KIND = {
  takeProfit: 1,
  stopLoss: 2,
} as const;

export type LeverageOrderKind = keyof typeof LEVERAGE_ORDER_KIND;

const ORDER_SEED = Buffer.from("leverage_order");

/**
 * A conditional order's address.
 *
 * Keyed by position, owner and order id, so one position can carry several
 * orders and two owners cannot collide on the same id.
 */
export function deriveLeverageOrderAddress(
  leveragePosition: PublicKey,
  owner: PublicKey,
  orderId: bigint | number | string,
  programId: PublicKey = LEVERAGE_DELEGATE_PROGRAM_ID
): [PublicKey, number] {
  const id = Buffer.alloc(8);
  id.writeBigUInt64LE(BigInt(orderId));
  return PublicKey.findProgramAddressSync(
    [ORDER_SEED, leveragePosition.toBuffer(), owner.toBuffer(), id],
    programId
  );
}

export interface CreateLeverageOrderParams {
  market: AddressLike;
  owner: AddressLike;
  /** Identifies the leverage position the order acts on. */
  positionId: AddressLike;
  orderId: bigint | number | string;
  kind: LeverageOrderKind;
  /** Trigger price in NAD; the order closes out when it is crossed. */
  triggerCloseoutPriceNad: bigint | number | string;
  /** How much of the position to close, in basis points. */
  closeBps: number;
  leveragePosition?: AddressLike;
  order?: AddressLike;
  remainingAccounts?: AccountMeta[];
}

/**
 * Conditional orders on leverage positions.
 *
 * These live in the delegate program rather than in Dusk itself, so they need
 * their own client. The position must already have delegated to this program
 * — see `buildCreateLeverageDelegationInstruction` — or the order has no
 * authority to act.
 */
export class DuskLeverageOrders {
  constructor(readonly program: LeverageDelegateProgram) {}

  async createOrderInstruction(
    params: CreateLeverageOrderParams
  ): Promise<TransactionInstruction> {
    const market = address(params.market);
    const owner = address(params.owner);
    const leveragePosition = address(
      params.leveragePosition ??
        deriveLeveragePositionAddress(market, address(params.positionId))[0]
    );

    if (!Number.isInteger(params.closeBps) || params.closeBps <= 0 || params.closeBps > 10_000) {
      throw new Error("Leverage order closeBps must be between 1 and 10000");
    }

    const order = address(
      params.order ??
        deriveLeverageOrderAddress(
          leveragePosition,
          owner,
          params.orderId,
          this.program.programId
        )[0]
    );

    return this.program.methods
      .createLeverageOrder({
        orderId: toBN(BigInt(params.orderId.toString())),
        kind: LEVERAGE_ORDER_KIND[params.kind],
        triggerCloseoutPriceNad: toBN(
          BigInt(params.triggerCloseoutPriceNad.toString())
        ),
        closeBps: params.closeBps,
      } as never)
      .accounts({
        market,
        leveragePosition,
        order,
        owner,
        systemProgram: SystemProgram.programId,
      } as never)
      .remainingAccounts(params.remainingAccounts ?? [])
      .instruction();
  }

  async createOrderTransaction(
    params: CreateLeverageOrderParams
  ): Promise<Transaction> {
    return new Transaction().add(await this.createOrderInstruction(params));
  }
}
