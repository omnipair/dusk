import { PublicKey, TransactionMessage, VersionedTransaction, type AddressLookupTableAccount, type TransactionInstruction } from "@solana/web3.js";
import { DEFAULT_PROTOCOL_REVENUE_SHARE_BPS, DUSK_PROGRAM_ID } from "./constants.js";

const BPS = 10_000n;
const U64_MAX = (1n << 64n) - 1n;
const min = (a: bigint, b: bigint) => a < b ? a : b;
const max = (a: bigint, b: bigint) => a > b ? a : b;
const ceilDiv = (a: bigint, b: bigint) => (a + b - 1n) / b;

export const LIQUIDATION_POLICY = Object.freeze({
  minimumDiscountBps: 50,
  maximumDiscountBps: 500,
  maximumEmergencyRewardBps: 100,
  emergencyMaintenanceFractionBps: 7_000,
  recoveryBufferBps: 200,
  solventFeeBps: 300,
  protocolFeeShareBps: DEFAULT_PROTOCOL_REVENUE_SHARE_BPS,
});

export interface LiquidationHealthInput {
  /** Linear symmetric-EMA value, in debt token atoms, after eligibility fees. */
  collateralValue: bigint;
  /** Indexed position debt in the same token atoms. */
  debt: bigint;
  maintenanceBps: number;
}

function amount(value: bigint, name: string): void {
  if (typeof value !== "bigint" || value < 0n || value > U64_MAX) {
    throw new RangeError(`${name} must be an unsigned u64 bigint`);
  }
}

/** Gates use exact products. Display equity is deliberately a separate value. */
export function liquidationHealth(input: LiquidationHealthInput) {
  amount(input.collateralValue, "collateralValue");
  amount(input.debt, "debt");
  if (!Number.isSafeInteger(input.maintenanceBps) || input.maintenanceBps <= 0 || input.maintenanceBps >= 10_000) {
    throw new RangeError("maintenanceBps must be between 1 and 9999");
  }
  const value = input.collateralValue;
  const equity = value - input.debt;
  const required = value * BigInt(input.maintenanceBps);
  const eligible = input.debt > 0n && (equity <= 0n || equity * BPS <= required);
  const emergencyAllowed = input.debt > 0n && (equity <= 0n || equity * BPS * 100n <= required * 70n);
  const distressProgressBps = input.debt === 0n ? 0n : equity <= 0n ? BPS : max(0n, required - equity * BPS) * BPS / required;
  const recoveryTargetReached = input.debt === 0n || (equity > 0n && equity * BPS >= value * BigInt(input.maintenanceBps + 200));
  return { eligible, emergencyAllowed, referenceInsolvent: input.debt > 0n && equity <= 0n, recoveryTargetReached, distressProgressBps: Number(distressProgressBps), equityAtoms: equity, equityBps: value === 0n ? null : Number(equity * BPS / value) };
}

/** Current health determines incentives; emergency and full-close gates remain separate. A partial quote may select a smaller affordable discount (at least
 * 50 bps). Use preview.rates.buyerDiscountBps for the actual payment terms.
 */
export function liquidationIncentives(input: LiquidationHealthInput) {
  const health = liquidationHealth(input);
  if (!health.eligible) throw new Error("Position is not liquidatable");
  const progress = BigInt(health.distressProgressBps);
  const maximumBuyerDiscountBps = Number(50n + (450n * progress / BPS));
  return {
    maximumBuyerDiscountBps,
    emergencyRewardBps: Number((100n * progress / BPS)),
    emergencyAllowed: health.emergencyAllowed,
  };
}

/** Net fee atoms. Physical transfer-fee gross-ups are additional, separate costs. */
export function liquidationFeeAllocation(netDebtRepaid: bigint, surplusCap = U64_MAX) {
  for (const [name, value] of Object.entries({ netDebtRepaid, surplusCap })) amount(value, name);
  const total = min(netDebtRepaid * 300n / BPS, surplusCap);
  const protocol = total * BigInt(LIQUIDATION_POLICY.protocolFeeShareBps) / BPS;
  return { total, protocol, junior: total - protocol };
}

export function liquidationLossAllocation(principal: bigint, debt: bigint, recovery: bigint) {
  for (const [name, value] of Object.entries({ principal, debt, recovery })) amount(value, name);
  if (principal > debt) throw new RangeError("Principal exceeds debt");
  const principalRepaid = min(principal, recovery);
  const interestPaid = min(debt - principal, max(0n, recovery - principal));
  return { principalRepaid, interestPaid, principalWrittenOff: principal - principalRepaid,
    interestCanceled: debt - principal - interestPaid, surplus: max(0n, recovery - debt) };
}

export function deriveLiquidationSessionAddress(position: PublicKey, programId = DUSK_PROGRAM_ID): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from("liquidation-session"), position.toBuffer()], programId);
}

/** Gross a route's final debt-token transfer up to the preview's required
 * escrow credit. This is the incoming transfer; the preview already budgets
 * outgoing custody transfers. Use the mint's effective fee for the current
 * epoch, and obtain a fresh quote if the epoch changes.
 */
export function grossLiquidationPayment(netCredit: bigint, fee: { basisPoints: number; maximumFee: bigint }): bigint {
  amount(netCredit, "netCredit");
  amount(fee.maximumFee, "maximumFee");
  if (!Number.isSafeInteger(fee.basisPoints) || fee.basisPoints < 0 || fee.basisPoints > 10_000) throw new RangeError("Invalid transfer fee");
  if (netCredit === 0n || fee.basisPoints === 0 || fee.maximumFee === 0n) return netCredit;
  const capped = netCredit + fee.maximumFee;
  const gross = fee.basisPoints === 10_000 ? capped : min(capped, ceilDiv(netCredit * BPS, BPS - BigInt(fee.basisPoints)));
  amount(gross, "grossPayment");
  return gross;
}

export function deriveLiquidationPaymentAddress(position: PublicKey, debtMint: PublicKey, programId = DUSK_PROGRAM_ID): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from("liquidation-payment"), position.toBuffer(), debtMint.toBuffer()], programId);
}

/** Compile the already-bound instruction list without prepending instructions
 * that would invalidate begin's settle index. Lookup tables must be active.
 * The caller simulates, signs and submits; this helper never sends funds.
 */
export function flashLiquidationV0Transaction(params: {
  payer: PublicKey; recentBlockhash: string; instructions: readonly TransactionInstruction[];
  lookupTables?: AddressLookupTableAccount[];
}): VersionedTransaction {
  const message = new TransactionMessage({ payerKey: params.payer, recentBlockhash: params.recentBlockhash, instructions: [...params.instructions] }).compileToV0Message(params.lookupTables ?? []);
  return new VersionedTransaction(message);
}
