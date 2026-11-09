/**
 * Compile-time checks for the leverage-delegate client: the generic path on
 * `DuskLeverageOrders`, its typed builders, and the IDL-generic types.
 *
 * `npm run typecheck` covers this file and never runs it. Each
 * `@ts-expect-error` line must fail to compile: if a change to the instruction
 * types lets one through, the directive is unused and the typecheck fails.
 * Each negative case stays on one line because the directive only covers the
 * line after it.
 */
import { BN } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";

import {
  DuskLeverageOrders,
  DuskProtectionOrders,
  type DuskIdl,
  type DuskInstructionArgs,
  type LeverageDelegateBuildOptions,
  type LeverageDelegateDefinedInput,
  type LeverageDelegateIdl,
  type LeverageDelegateInstructionAccounts,
  type LeverageDelegateInstructionArgs,
  type LeverageDelegateProgram,
  type ProgramDefinedInput,
  type ProgramInstructionAccounts,
  type ProgramInstructionArgs,
} from "../src/index.js";

declare const program: LeverageDelegateProgram;
const orders = new DuskLeverageOrders(program);
const key = PublicKey.default;

const entryArgs = {
  orderId: 1n,
  positionId: key,
  debtAsset: 1,
  depositAmount: 1_000n,
  protocolFeeDepositAmount: 2n,
  minMarginAmount: "900",
  executorBounty: 10,
  multiplierBps: new BN(20_000),
  limitPriceNad: 1n,
  minCollateralOut: 1n,
  expiryUnixTimestamp: 1n,
  referrer: null,
};
const { protocolFeeDepositAmount, ...withoutFeeDeposit } = entryArgs;
void protocolFeeDepositAmount;
const entryAccounts = { market: key, debtMint: key, collateralMint: key, ownerFundingAccount: key, fundingVault: key };
// An existing order's PDA is seeded by its own data (`order.owner`), so
// Anchor cannot derive it: every instruction that acts on one names it.
const cancelEntryAccounts = { order: key, debtMint: key, fundingVault: key, ownerFundingAccount: key };
const { order: cancelEntryOrder, ...cancelEntryWithoutOrder } = cancelEntryAccounts;
void cancelEntryOrder;
const protectionAccounts = { order: key, lpMint: key, custodyLpAccount: key, ownerLpAccount: key };
const executeHlpAccounts = {
  order: key,
  market: key,
  futarchyAuthority: key,
  baseMint: key,
  quoteMint: key,
  srlpMint: key,
  targetHlpMint: key,
  baseReserveVault: key,
  quoteReserveVault: key,
  borrowedInterestVault: key,
  custodyTargetAccount: key,
  custodyHlpAccount: key,
  hlpSrlpAccount: key,
  baseYieldAccount: key,
  quoteYieldAccount: key,
  ownerTargetAccount: key,
  executorTargetAccount: key,
  orderOwner: key,
};
const { order: executeHlpOrder, ...executeHlpWithoutOrder } = executeHlpAccounts;
void executeHlpOrder;

export async function genericPath(): Promise<void> {
  // Integers in any form; Anchor resolves the order PDA, the signer and the programs.
  await orders.instruction("createLeverageEntryOrder", entryArgs, { accounts: entryAccounts, remainingAccounts: [] });
  await orders.transaction("createLeverageEntryOrder", [entryArgs], { accounts: entryAccounts });
  orders.builder("createLeverageEntryOrder", entryArgs, { accounts: entryAccounts });
  orders.method("createLeverageEntryOrder", entryArgs);
  // An Option may be left out.
  const { referrer, ...withoutReferrer } = entryArgs;
  void referrer;
  await orders.rpc("createLeverageEntryOrder", withoutReferrer, { accounts: entryAccounts });

  // @ts-expect-error the fee deposit is required; dusk-webapp PR #98 sent it as zero
  await orders.instruction("createLeverageEntryOrder", withoutFeeDeposit, { accounts: entryAccounts });
  // @ts-expect-error a misspelled field
  await orders.instruction("createLeverageEntryOrder", { ...entryArgs, protocolFeeDepositAmout: 2n }, { accounts: entryAccounts });
  // @ts-expect-error a u8 is a number
  await orders.instruction("createLeverageEntryOrder", { ...entryArgs, debtAsset: 1n }, { accounts: entryAccounts });
  // @ts-expect-error a public key is not a number
  await orders.instruction("createLeverageEntryOrder", { ...entryArgs, positionId: 1 }, { accounts: entryAccounts });
  // @ts-expect-error arguments are required
  await orders.instruction("createLeverageEntryOrder");
  // @ts-expect-error arguments are required by `method` too
  orders.method("createLeverageEntryOrder");
  // @ts-expect-error so are the accounts Anchor cannot resolve
  await orders.instruction("createLeverageEntryOrder", entryArgs);
  // @ts-expect-error a misspelled account
  await orders.instruction("createLeverageEntryOrder", entryArgs, { accounts: { ...entryAccounts, ownerFundingAcount: key } });
  // @ts-expect-error a required account is missing
  await orders.instruction("createLeverageEntryOrder", entryArgs, { accounts: { market: key, debtMint: key, collateralMint: key, fundingVault: key } });
  // @ts-expect-error a Dusk instruction is not a delegate instruction
  await orders.instruction("borrow", { borrowAmount: 1n, minDebtAmountOut: 1n, minLiquidationCfBps: 0 }, { accounts: entryAccounts });

  // The other delegate calls dusk-webapp builds fit the same path.
  const hlpOrder: LeverageDelegateDefinedInput<"createHlpOrderArgs"> = { orderId: 1n, kind: 1, hlpAmount: 1n, triggerNad: 1n, minTargetAmountOut: 1n };
  await orders.instruction("createHlpOrder", hlpOrder, { accounts: { market: key, targetHlpMint: key, baseMint: key, quoteMint: key, ownerHlpAccount: key, custodyHlpAccount: key, baseYieldAccount: key, quoteYieldAccount: key } });
  await orders.instruction("updateLeverageOrder", { orderId: 1n, kind: 2, triggerCloseoutPriceNad: 1n, closeBps: 10_000 }, { accounts: { market: key, leveragePosition: key } });
  await orders.instruction("cancelHlpOrder", { orderId: 1n }, { accounts: { order: key, targetHlpMint: key, custodyHlpAccount: key, ownerHlpAccount: key } });
  await orders.instruction("cancelLeverageEntryOrder", { orderId: 1n, minOwnerRefundOut: 0n }, { accounts: cancelEntryAccounts });
  // @ts-expect-error the refund floor is required
  await orders.instruction("cancelLeverageEntryOrder", { orderId: 1n }, { accounts: cancelEntryAccounts });
  // @ts-expect-error the order is required: its seeds read its own data
  await orders.instruction("cancelLeverageEntryOrder", { orderId: 1n, minOwnerRefundOut: 0n }, { accounts: cancelEntryWithoutOrder });
  // @ts-expect-error on every instruction that acts on an existing order
  await orders.instruction("cancelHlpOrder", { orderId: 1n }, { accounts: { targetHlpMint: key, custodyHlpAccount: key, ownerHlpAccount: key } });
  await orders.instruction("cancelLeverageOrder", { orderId: 1n }, { accounts: { order: key } });
  // @ts-expect-error so a cancel whose other accounts all resolve still takes options
  await orders.instruction("cancelLeverageOrder", { orderId: 1n });
  // @ts-expect-error the hLP trigger is triggerNad
  await orders.instruction("createHlpOrder", { orderId: 1n, kind: 1, hlpAmount: 1n, triggerNAD: 1n, minTargetAmountOut: 1n }, { accounts: { market: key, targetHlpMint: key, baseMint: key, quoteMint: key, ownerHlpAccount: key, custodyHlpAccount: key, baseYieldAccount: key, quoteYieldAccount: key } });

  // An instruction without arguments takes none.
  await orders.instruction("cancelProtectionOrder", undefined, { accounts: protectionAccounts });
  await orders.instruction("previewProtectionOrder", [], { accounts: { order: key, market: key, borrowPosition: null, leveragePosition: key, collateralMint: key } });
  // @ts-expect-error it takes no arguments
  await orders.instruction("cancelProtectionOrder", { orderId: 1n }, { accounts: protectionAccounts });

  // Nested account groups are nested objects.
  await orders.instruction("executeHlpOrder", { orderId: 1n }, { accounts: { ...executeHlpAccounts, protocolFee: { feeRecipient: key } } });
  // @ts-expect-error a misspelled nested account
  await orders.instruction("executeHlpOrder", { orderId: 1n }, { accounts: { ...executeHlpAccounts, protocolFee: { feeRecipent: key } } });
  // @ts-expect-error the protocol fee group is required
  await orders.instruction("executeHlpOrder", { orderId: 1n }, { accounts: executeHlpAccounts });
  // @ts-expect-error and so is the order
  await orders.instruction("executeHlpOrder", { orderId: 1n }, { accounts: { ...executeHlpWithoutOrder, protocolFee: { feeRecipient: key } } });
  // @ts-expect-error null means "none" only for an optional account
  await orders.instruction("executeHlpOrder", { orderId: 1n }, { accounts: { ...executeHlpAccounts, protocolFee: { feeRecipient: key }, orderOwner: null } });

  // Options typed for one instruction do not fit another.
  const entryOptions: LeverageDelegateBuildOptions<"createLeverageEntryOrder"> = {};
  // @ts-expect-error entry-order options are not hLP-order options
  const hlpOptions: LeverageDelegateBuildOptions<"createHlpOrder"> = entryOptions;
  void hlpOptions;
}

export async function typedBuilders(): Promise<void> {
  await orders.createOrderInstruction({ market: key, owner: key, positionId: key, orderId: 1n, kind: "takeProfit", triggerCloseoutPriceNad: "1", closeBps: 10_000 });
  await orders.cancelOrderInstruction({ owner: key, orderId: 1, order: key });
  // @ts-expect-error the kind is "takeProfit" or "stopLoss"
  await orders.createOrderInstruction({ market: key, owner: key, positionId: key, orderId: 1n, kind: "takeprofit", triggerCloseoutPriceNad: 1n, closeBps: 10_000 });
  // @ts-expect-error the trigger is required
  await orders.createOrderInstruction({ market: key, owner: key, positionId: key, orderId: 1n, kind: "stopLoss", closeBps: 10_000 });

  const protection = new DuskProtectionOrders(program);
  const protectionParams = { orderId: 1n, action: 0, debtAsset: 1, lpAmount: 1n, maxLpPerExecution: 1n, maxPaymentPerExecution: 1n, triggerHealthBps: 1n, targetHealthBps: 1n, minPaymentPerLpNad: 1n, keeperFeeBps: 30, expiresAt: 1n };
  const createAccounts = { market: key, borrowPosition: key, leveragePosition: null, lpMint: key, baseMint: key, quoteMint: key, ownerLpAccount: key, custodyLpAccount: key, baseYieldAccount: key, quoteYieldAccount: key, owner: key };
  await protection.createInstruction(protectionParams, createAccounts);
  const { debtAsset, ...withoutDebtAsset } = protectionParams;
  void debtAsset;
  // @ts-expect-error the debt side is required
  await protection.createInstruction(withoutDebtAsset, createAccounts);
}

// The aliases are the IDL-generic types applied to each program.
type Same<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;
export const sameDelegateArgs: Same<
  ProgramInstructionArgs<LeverageDelegateIdl, "createLeverageEntryOrder">,
  LeverageDelegateInstructionArgs<"createLeverageEntryOrder">
> = true;
export const sameDelegateAccounts: Same<
  ProgramInstructionAccounts<LeverageDelegateIdl, "executeHlpOrder">,
  LeverageDelegateInstructionAccounts<"executeHlpOrder">
> = true;
export const sameDuskArgs: Same<ProgramInstructionArgs<DuskIdl, "borrow">, DuskInstructionArgs<"borrow">> = true;
export const sameDefined: Same<
  ProgramDefinedInput<LeverageDelegateIdl, "createHlpOrderArgs">,
  LeverageDelegateDefinedInput<"createHlpOrderArgs">
> = true;

// Exactly the accounts Anchor cannot fill in are required. An existing order
// is; a new one, whose PDA is seeded by the arguments, is not. On the Dusk
// side the market and the proposal read their own data, and the market's
// relation does not help; `rentRefund`, a relation of the proposal, resolves.
type RequiredKeys<T> = { [K in keyof T]-?: {} extends Pick<T, K> ? never : K }[keyof T];
export const cancelRequires: Same<RequiredKeys<LeverageDelegateInstructionAccounts<"cancelLeverageOrder">>, "order"> = true;
export const createRequires: Same<
  RequiredKeys<LeverageDelegateInstructionAccounts<"createLeverageOrder">>,
  "market" | "leveragePosition"
> = true;
export const resolveOperatorChangeRequires: Same<
  RequiredKeys<ProgramInstructionAccounts<DuskIdl, "resolveOperatorChange">>,
  "market" | "proposal"
> = true;

declare const duskBorrow: DuskInstructionArgs<"borrow">;
// @ts-expect-error Dusk arguments are not delegate arguments
export const notEntryArgs: LeverageDelegateInstructionArgs<"createLeverageEntryOrder"> = duskBorrow;
declare const hlpOrderAccounts: LeverageDelegateInstructionAccounts<"createHlpOrder">;
// @ts-expect-error one delegate instruction's accounts are not another's
export const notEntryAccounts: LeverageDelegateInstructionAccounts<"createLeverageEntryOrder"> = hlpOrderAccounts;
// @ts-expect-error not a delegate type
export type NotADelegateType = LeverageDelegateDefinedInput<"swapArgs">;
