import { AnchorProvider, Program, type BN } from "@coral-xyz/anchor";
import {
  PublicKey,
  SystemProgram,
  Transaction,
  type AccountMeta,
  type TransactionInstruction,
} from "@solana/web3.js";

import { address, type AddressLike } from "./address.js";
import { deriveLeveragePositionAddress, DUSK_PROGRAM_ID } from "./constants.js";
import DELEGATE_IDL from "./idl_delegate.js";
import {
  DuskInstructionInputError,
  normalizeInstructionAccounts,
  normalizeInstructionArgs,
  type LeverageDelegateInstructionAccounts,
  type LeverageDelegateInstructionArgs,
  type LeverageDelegateInstructionName,
  type ProgramBuildCall,
  type ProgramMethodCall,
} from "./instruction-args.js";
import type { LeverageDelegate } from "./types_delegate.js";
import { getAssociatedTokenAddressSync, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { integerBN } from "./parameters.js";
import type { RawAmount } from "./write.js";

/** Hard-coded additional fee on successful built-in order execution. */
export const ORDER_PROTOCOL_FEE_BPS = 10n;

/** Net protocol fee in raw units, rounded up. Token-2022 transfer fees are additional. */
export function calculateOrderProtocolFee(executedValue: RawAmount): bigint {
  const value = BigInt(integerBN(executedValue, "executedValue").toString());
  if (value < 0n || value > 0xffffffffffffffffn) throw new RangeError("executedValue must fit u64");
  return (value * ORDER_PROTOCOL_FEE_BPS + 9_999n) / 10_000n;
}

/** Average execution price in whole debt tokens per whole collateral token, scaled by 1e9. */
export function calculateLeverageCloseoutPrice(value: RawAmount, collateral: RawAmount,
  debtDecimals: number, collateralDecimals: number): bigint {
  const output = BigInt(integerBN(value, "closeoutValue").toString());
  const input = BigInt(integerBN(collateral, "collateralAmount").toString());
  if (input === 0n) throw new RangeError("collateralAmount must be positive");
  for (const decimals of [debtDecimals, collateralDecimals]) {
    if (!Number.isSafeInteger(decimals) || decimals < 0 || decimals > 255) throw new RangeError("Invalid token decimals");
  }
  const difference = collateralDecimals - debtDecimals;
  const scale = 10n ** BigInt(Math.abs(difference));
  const factor = 1_000_000_000n * (difference >= 0 ? scale : 1n);
  const numerator = output * factor;
  const denominator = input * (difference < 0 ? scale : 1n);
  const maximum = (1n << 128n) - 1n;
  if (scale > maximum || factor > maximum || numerator > maximum || denominator > maximum)
    throw new RangeError("Closeout price arithmetic exceeds u128");
  return BigInt(integerBN(numerator / denominator, "closeoutPriceNad").toString());
}

/** Supply under `protocolFee` when executing any built-in order. Create this
 * ATA if needed. Treasury comes from Dusk's canonical FutarchyAuthority.
 */
export function orderProtocolFeeAccounts(treasury: PublicKey, mint: PublicKey,
  tokenProgram = TOKEN_PROGRAM_ID) {
  return { feeRecipient: getAssociatedTokenAddressSync(mint, treasury, true, tokenProgram) };
}

export const LEVERAGE_DELEGATE_PROGRAM_ID = address(
  "5yeZJw1bNer5jq4GVNa7ifsgnM7ivxXQLCK6ro8EJ3Ue"
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

// An unknown kind used to index LEVERAGE_ORDER_KIND as undefined, which
// Anchor encoded as kind 0.
function leverageOrderKindValue(kind: LeverageOrderKind): number {
  if (!Object.prototype.hasOwnProperty.call(LEVERAGE_ORDER_KIND, kind)) {
    const names = Object.keys(LEVERAGE_ORDER_KIND).map((name) => `"${name}"`).join(" or ");
    const received = typeof kind === "string" ? `string ${JSON.stringify(kind)}` : String(kind);
    throw new DuskInstructionInputError(
      "createLeverageOrder",
      "kind",
      `"kind" must be ${names}, received ${received}`,
      "Leverage delegate"
    );
  }
  return LEVERAGE_ORDER_KIND[kind];
}

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
  namespaceAuthority?: AddressLike;
  /** Identifies the leverage position the order acts on. */
  positionId: AddressLike;
  orderId: bigint | number | string;
  kind: LeverageOrderKind;
  /** Symmetric EMA threshold in whole debt tokens per collateral token, scaled by 1e9. */
  triggerPriceNad: bigint | number | string;
  /** How much of the position to close, in basis points. */
  closeBps: number;
  /** Positive minimum net payout in raw debt-token units, after debt and fees. */
  minOwnerPayout: RawAmount;
  leveragePosition?: AddressLike;
  order?: AddressLike;
  remainingAccounts?: AccountMeta[];
}

interface LeverageDelegateBuildOptionsBase {
  /** Appended after the instruction's accounts, such as transfer-hook or oracle accounts. */
  remainingAccounts?: AccountMeta[];
}

/** Options for the generic leverage-delegate builders on `DuskLeverageOrders`. */
export interface LeverageDelegateBuildOptions<
  out N extends LeverageDelegateInstructionName = LeverageDelegateInstructionName,
> extends LeverageDelegateBuildOptionsBase {
  accounts?: LeverageDelegateInstructionAccounts<N>;
}

type LeverageDelegateBuildOptionsWithAccounts<N extends LeverageDelegateInstructionName> =
  LeverageDelegateBuildOptionsBase & {
    accounts: LeverageDelegateInstructionAccounts<N>;
  };

/**
 * The arguments after the instruction name in `DuskLeverageOrders#builder`,
 * `instruction`, `transaction` and `rpc`. `args` is required unless the
 * instruction takes none; `options` and its `accounts` are required when the
 * instruction has an account Anchor cannot resolve on its own.
 */
export type LeverageDelegateBuildCall<N extends LeverageDelegateInstructionName> = ProgramBuildCall<
  LeverageDelegate,
  N,
  LeverageDelegateBuildOptions<N>,
  LeverageDelegateBuildOptionsWithAccounts<N>
>;

/** The argument after the instruction name in `DuskLeverageOrders#method`. */
export type LeverageDelegateMethodCall<N extends LeverageDelegateInstructionName> = ProgramMethodCall<
  LeverageDelegate,
  N
>;

type AnchorMethodBuilder = {
  accounts(accounts: Record<string, unknown>): AnchorMethodBuilder;
  remainingAccounts(accounts: AccountMeta[]): AnchorMethodBuilder;
  preInstructions(instructions: TransactionInstruction[]): AnchorMethodBuilder;
  instruction(): Promise<TransactionInstruction>;
  transaction(): Promise<Transaction>;
  rpc(): Promise<string>;
};

type AnchorMethods = Record<string, (...args: unknown[]) => AnchorMethodBuilder>;

// What the public signatures narrow down to once the name is known.
interface UncheckedBuildOptions extends LeverageDelegateBuildOptionsBase {
  accounts?: Record<string, unknown>;
}

export interface CancelLeverageOrderParams {
  owner: AddressLike;
  namespaceAuthority?: AddressLike;
  orderId: bigint | number | string;
  /** Identifies the leverage position the order acts on. */
  positionId?: AddressLike;
  /** Required when `positionId` is not given. */
  market?: AddressLike;
  leveragePosition?: AddressLike;
  /** Supply the order address directly to skip derivation entirely. */
  order?: AddressLike;
}

/**
 * Orders in the leverage-delegate program: conditional exits on leverage
 * positions through typed builders, and every other delegate instruction
 * (entry orders, hLP orders, keeper execution) through the generic
 * `method`, `builder`, `instruction`, `transaction` and `rpc`.
 *
 * These live in the delegate program rather than in Dusk itself, so they need
 * their own client. The position must already have delegated to this program
 * — see `buildCreateLeverageDelegationInstruction` — or the order has no
 * authority to act.
 *
 * Arguments and account names are checked against the delegate IDL before
 * anything is encoded: a missing, misspelled or wrongly typed field, or an
 * account the instruction does not declare, throws `DuskInstructionInputError`
 * instead of reaching the program as zero.
 */
export class DuskLeverageOrders {
  /** `duskProgramId` is the Dusk deployment whose leverage positions the orders act on. */
  constructor(readonly program: LeverageDelegateProgram, readonly duskProgramId: PublicKey = DUSK_PROGRAM_ID) {}

  /**
   * Anchor's method builder for one delegate instruction, with `args` checked
   * against the IDL and converted for Anchor.
   */
  method<N extends LeverageDelegateInstructionName>(
    name: N,
    ...call: LeverageDelegateMethodCall<N>
  ): AnchorMethodBuilder {
    return this.anchorMethod(name, normalizeInstructionArgs(this.program.idl, name, call[0]));
  }

  /**
   * Anchor's method builder with accounts and remaining accounts applied.
   * Arguments and account names are checked against the IDL; accounts Anchor
   * can resolve (PDAs, fixed programs, signers) may be left out. Oracle and
   * transfer-hook accounts go in `remainingAccounts`.
   */
  builder<N extends LeverageDelegateInstructionName>(
    name: N,
    ...call: LeverageDelegateBuildCall<N>
  ): AnchorMethodBuilder {
    const [args, options] = call as [unknown, UncheckedBuildOptions?];
    return this.anchorBuilder(name, normalizeInstructionArgs(this.program.idl, name, args), options);
  }

  async instruction<N extends LeverageDelegateInstructionName>(
    name: N,
    ...call: LeverageDelegateBuildCall<N>
  ): Promise<TransactionInstruction> {
    return this.builder(name, ...call).instruction();
  }

  async transaction<N extends LeverageDelegateInstructionName>(
    name: N,
    ...call: LeverageDelegateBuildCall<N>
  ): Promise<Transaction> {
    return this.builder(name, ...call).transaction();
  }

  async rpc<N extends LeverageDelegateInstructionName>(
    name: N,
    ...call: LeverageDelegateBuildCall<N>
  ): Promise<string> {
    return this.builder(name, ...call).rpc();
  }

  private anchorMethod(name: LeverageDelegateInstructionName, args: unknown[]): AnchorMethodBuilder {
    const method = (this.program.methods as unknown as AnchorMethods)[name];
    if (!method) {
      throw new Error(`Unknown Leverage delegate instruction: ${name}`);
    }
    return method(...args);
  }

  private anchorBuilder(
    name: LeverageDelegateInstructionName,
    args: unknown[],
    options: UncheckedBuildOptions = {}
  ): AnchorMethodBuilder {
    let builder = this.anchorMethod(name, args);
    if (options.accounts) {
      builder = builder.accounts(normalizeInstructionAccounts(this.program.idl, name, options.accounts));
    }
    if (options.remainingAccounts?.length) {
      builder = builder.remainingAccounts(options.remainingAccounts);
    }
    return builder;
  }

  async createOrderInstruction(
    params: CreateLeverageOrderParams
  ): Promise<TransactionInstruction> {
    const market = address(params.market);
    const owner = address(params.owner);

    if (!Number.isInteger(params.closeBps) || params.closeBps <= 0 || params.closeBps > 10_000) {
      throw new Error("Leverage order closeBps must be between 1 and 10000");
    }

    // Checked before the order address is derived from orderId.
    const args = normalizeInstructionArgs(this.program.idl, "createLeverageOrder", {
      orderId: params.orderId,
      kind: leverageOrderKindValue(params.kind),
      triggerPriceNad: params.triggerPriceNad,
      closeBps: params.closeBps,
      minOwnerPayout: params.minOwnerPayout,
    } satisfies LeverageDelegateInstructionArgs<"createLeverageOrder">);

    const leveragePosition = address(
      params.leveragePosition ??
        deriveLeveragePositionAddress(market, owner, address(params.positionId), address(params.namespaceAuthority ?? params.owner), this.duskProgramId)[0]
    );
    const order = address(
      params.order ??
        deriveLeverageOrderAddress(
          leveragePosition,
          owner,
          checkedOrderId(args),
          this.program.programId
        )[0]
    );

    return this.anchorBuilder("createLeverageOrder", args, {
      accounts: {
        market,
        leveragePosition,
        order,
        owner,
        systemProgram: SystemProgram.programId,
      } satisfies LeverageDelegateInstructionAccounts<"createLeverageOrder">,
      remainingAccounts: params.remainingAccounts,
    }).instruction();
  }

  async createOrderTransaction(
    params: CreateLeverageOrderParams
  ): Promise<Transaction> {
    return new Transaction().add(await this.createOrderInstruction(params));
  }

  /**
   * Cancel a conditional order and return its rent to the owner.
   *
   * The program resolves the order's position from the account itself, so
   * cancelling needs neither the market nor the position — but the address
   * still has to be derived from them unless `order` is supplied.
   */
  async cancelOrderInstruction(
    params: CancelLeverageOrderParams
  ): Promise<TransactionInstruction> {
    const owner = address(params.owner);
    const args = normalizeInstructionArgs(this.program.idl, "cancelLeverageOrder", {
      orderId: params.orderId,
    } satisfies LeverageDelegateInstructionArgs<"cancelLeverageOrder">);
    const order = address(params.order ?? this.resolveOrderAddress(params, checkedOrderId(args)));

    return this.anchorBuilder("cancelLeverageOrder", args, {
      accounts: { order, owner } satisfies LeverageDelegateInstructionAccounts<"cancelLeverageOrder">,
    }).instruction();
  }

  async cancelOrderTransaction(
    params: CancelLeverageOrderParams
  ): Promise<Transaction> {
    return new Transaction().add(await this.cancelOrderInstruction(params));
  }

  private resolveOrderAddress(params: CancelLeverageOrderParams, orderId: string): PublicKey {
    const leveragePosition = params.leveragePosition
      ? address(params.leveragePosition)
      : (() => {
          if (!params.market || !params.positionId) {
            throw new Error(
              "Cancelling an order needs `order`, or `leveragePosition`, or both `market` and `positionId`"
            );
          }
          return deriveLeveragePositionAddress(
            address(params.market),
            address(params.owner),
            address(params.positionId),
            address(params.namespaceAuthority ?? params.owner),
            this.duskProgramId
          )[0];
        })();
    return deriveLeverageOrderAddress(
      leveragePosition,
      address(params.owner),
      orderId,
      this.program.programId
    )[0];
  }
}

// The decimal order id from checked `{ orderId }` arguments.
function checkedOrderId(args: unknown[]): string {
  return (args[0] as { orderId: BN }).orderId.toString(10);
}
