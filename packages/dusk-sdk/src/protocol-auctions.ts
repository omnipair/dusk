import IDL from "./idl_v2.js";
import type { IntegerLike } from "./parameters.js";

const U64 = (1n << 64n) - 1n;
const U128 = (1n << 128n) - 1n;
/** Uses Dusk's nominal slot cadence, rather than measured wall-clock days. */
export const PROTOCOL_AUCTION_SLOTS_PER_DAY = 86_400_000n / BigInt(
  IDL.constants.find(({ name }) => name === "TARGET_MS_PER_SLOT")!.value,
);

export interface ProtocolAuctionCurveInput {
  virtualSoldReserve: IntegerLike;
  virtualPaymentReserve: IntegerLike;
  soldRatePerDay: IntegerLike;
  lastUpdateSlot: IntegerLike;
  streamRemainder: IntegerLike;
}

function integer(input: IntegerLike, max: bigint): bigint {
  if (typeof input === "number" && !Number.isSafeInteger(input)) throw new Error("Auction integer lost precision");
  if (!/^(0|[1-9]\d*)$/.test(String(input))) throw new Error("Invalid auction integer");
  const result = BigInt(String(input));
  if (result > max) throw new Error("Auction integer overflow");
  return result;
}
const ceilDiv = (numerator: bigint, denominator: bigint) => (numerator + denominator - 1n) / denominator;

/** Exact-output quote for the continuous virtual constant-product curve.
 * Amounts are raw token atoms. soldNetAmount is the bidder's post-transfer-fee
 * credit; settlement retires the gross custody debit separately. This quote
 * does not establish available fee inventory, mint support, or account validity.
 * Fetch the selected market side/lane/source and slot coherently before use.
 */
export function quoteProtocolAuction(input: {
  auction: ProtocolAuctionCurveInput;
  soldNetAmount: IntegerLike;
  currentSlot: IntegerLike;
  stakingVaultBps?: number;
}) {
  const { auction } = input;
  const sold = integer(auction.virtualSoldReserve, U128);
  const payment = integer(auction.virtualPaymentReserve, U128);
  const rate = integer(auction.soldRatePerDay, U64);
  const lastSlot = integer(auction.lastUpdateSlot, U64);
  const slot = integer(input.currentSlot, U64);
  const remainder = integer(auction.streamRemainder, PROTOCOL_AUCTION_SLOTS_PER_DAY - 1n);
  const amount = integer(input.soldNetAmount, U64);
  const bps = input.stakingVaultBps ?? 0;
  if (!Number.isInteger(bps) || bps < 0 || bps > 10_000) throw new Error("Invalid auction distribution");
  if (sold === 0n || payment === 0n || amount === 0n) throw new Error("Auction reserves and output must be positive");
  if (slot < lastSlot) throw new Error("Invalid auction clock");
  const streamed = integer(rate * (slot - lastSlot) + remainder, U128);
  const virtualSold = integer(sold + streamed / PROTOCOL_AUCTION_SLOTS_PER_DAY, U128);
  const virtualPayment = ceilDiv(payment * sold, virtualSold);
  if (amount >= virtualSold) throw new Error("Insufficient virtual auction inventory");
  const remainingSold = virtualSold - amount;
  const paymentAmount = integer(ceilDiv(virtualPayment * amount, remainingSold), U64);
  const stakingVaultAmount = paymentAmount * BigInt(bps) / 10_000n;
  return {
    paymentAmount, stakingVaultAmount, treasuryAmount: paymentAmount - stakingVaultAmount,
    nextState: {
      virtualSoldReserve: remainingSold,
      virtualPaymentReserve: integer(virtualPayment + paymentAmount, U128),
      soldRatePerDay: rate, lastUpdateSlot: slot,
      streamRemainder: streamed % PROTOCOL_AUCTION_SLOTS_PER_DAY,
    },
  };
}
