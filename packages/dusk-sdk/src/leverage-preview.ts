import { utils, type Program } from "@coral-xyz/anchor";
import { PublicKey, type TransactionInstruction } from "@solana/web3.js";
import { address, type AddressLike } from "./address.js";
import { leverageActionMetrics } from "./leverage-actions.js";
import { getStoredLeverageMargins } from "./leverage-margins.js";
import { simulatePreviewWithContext, type PreviewSimulationOptions } from "./simulation.js";
import type { LeveragePosition, Market } from "./type-aliases.js";
import type { Dusk } from "./types_v2.js";

export interface PreviewLeverageActionParams extends PreviewSimulationOptions {
  market: AddressLike;
  leveragePosition: AddressLike;
  /** The actual action and optional setup instructions, in execution order.
   * Simulation does not sign, submit, or move funds. Wallet accounts must be
   * funded in the simulated bank, just as for the real transaction. */
  instructions: readonly TransactionInstruction[];
}

/** Authoritative simulation of the complete owner action, including hLP,
 * current interest, mint fees, new admission terms and atomic failure. All
 * returned accounts/events come from the same simulated bank. EMA metrics
 * are explicitly mark valuations, not execution proceeds or liquidation quotes.
 */
export async function previewLeverageAction(program: Program<Dusk>, params: PreviewLeverageActionParams) {
  const marketKey = address(params.market), positionKey = address(params.leveragePosition);
  const simulation = await simulatePreviewWithContext(program, params.instructions, {
    ...params, accounts: [marketKey, positionKey], innerInstructions: true, requireReturnData: false,
  });
  const [marketInfo, positionInfo] = simulation.value.accounts!;
  if (!marketInfo || marketInfo.owner !== program.programId.toBase58()) throw new Error("Missing Dusk market after simulation");
  const market = program.coder.accounts.decode<Market>("market", Buffer.from(marketInfo.data[0], "base64"));
  let position: LeveragePosition | null = null;
  if (positionInfo && positionInfo.lamports > 0) {
    if (positionInfo.owner !== program.programId.toBase58()) throw new Error("Unexpected simulated position owner");
    position = program.coder.accounts.decode<LeveragePosition>("leveragePosition", Buffer.from(positionInfo.data[0], "base64"));
    if (!position.market.equals(marketKey)) throw new Error("Position belongs to another market");
  }
  const flows = new Map<string, { mint: PublicKey; debit: bigint; credit: bigint }>();
  const flow = (mint: PublicKey, debit: bigint, credit: bigint) => {
    const key = mint.toBase58();
    const old = flows.get(key) ?? { mint, debit: 0n, credit: 0n };
    flows.set(key, { mint, debit: old.debit + debit, credit: old.credit + credit });
  };
  let matched = 0;
  for (const group of simulation.value.innerInstructions ?? []) for (const ix of group.instructions) {
    // RPCs return either explicit program IDs or compiled indexes for inner instructions.
    const raw = ix as unknown as { programId?: string | PublicKey; programIdIndex?: number; data?: string };
    const programId = raw.programId ? new PublicKey(raw.programId)
      : raw.programIdIndex === undefined ? undefined : simulation.accountKeys[raw.programIdIndex];
    if (!programId?.equals(program.programId) || !raw.data) continue;
    const data = Buffer.from(utils.bytes.bs58.decode(raw.data));
    if (data.length < 16 || !data.subarray(0, 8).equals(Buffer.from("e445a52e51cb9a1d", "hex"))) continue;
    const event = program.coder.events.decode(data.subarray(8).toString("base64"));
    const value = event?.data as Record<string, unknown> | undefined;
    if (!event || !value || !(value.position instanceof PublicKey) || !value.position.equals(positionKey)) continue;
    if (!(value.market instanceof PublicKey) || !value.market.equals(marketKey)) throw new Error("Mismatched leverage event market");
    const amount = (field: string) => BigInt(String(value[field] ?? 0));
    if (event.name === "leveragePositionUpdated") {
      flow(value.debtAssetMint as PublicKey, amount("ownerDebit"), amount("ownerCredit")); matched++;
    } else if (event.name === "leveragePositionClosed") {
      flow(value.debtAssetMint as PublicKey, 0n, amount("residual")); matched++;
    } else if (event.name === "debtFreePositionClosed") {
      flow(market.baseSide.assetMint, 0n, amount("baseReceived"));
      flow(market.quoteSide.assetMint, 0n, amount("quoteReceived")); matched++;
    }
  }
  if (!matched) throw new Error("RPC did not return the position action's CPI events; cannot quote wallet cash flows");
  let metrics = null;
  if (position) {
    const debtIsBase = position.debtAsset === 0;
    const heldSide = debtIsBase ? market.quoteSide : market.baseSide;
    const debtSide = debtIsBase ? market.baseSide : market.quoteSide;
    const price = BigInt((debtIsBase ? market.risk.quotePriceEmaNad : market.risk.basePriceEmaNad).toString());
    const index = BigInt((debtIsBase ? market.debt.baseBorrowIndexNad : market.debt.quoteBorrowIndexNad).toString());
    const collateralAmount = BigInt(position.collateralAmount.toString());
    const collateralValue = collateralAmount * price * 10n ** BigInt(debtSide.assetDecimals)
      / (1_000_000_000n * 10n ** BigInt(heldSide.assetDecimals));
    metrics = leverageActionMetrics({ collateralAmount, collateralValue,
      debtAmount: BigInt(position.debtShares.toString()) * index / 1_000_000_000n });
  }
  return { slot: simulation.context.slot, observedAt: simulation.observedAt,
    unitsConsumed: simulation.value.unitsConsumed, logs: simulation.value.logs,
    market, position, closed: position === null, walletFlows: [...flows.values()],
    priceBasis: "symmetric-ema" as const, metrics,
    marginTerms: position ? getStoredLeverageMargins(position) : null };
}
