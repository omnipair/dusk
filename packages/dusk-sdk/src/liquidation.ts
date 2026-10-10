import { PublicKey, TransactionMessage, VersionedTransaction, type AddressLookupTableAccount, type TransactionInstruction } from "@solana/web3.js";
import { DUSK_PROGRAM_ID } from "./constants.js";

const BPS = 10_000n;
const U64_MAX = (1n << 64n) - 1n;
const min = (a: bigint, b: bigint) => a < b ? a : b;
const max = (a: bigint, b: bigint) => a > b ? a : b;
const ceilDiv = (a: bigint, b: bigint) => (a + b - 1n) / b;

export const LIQUIDATION_POLICY = Object.freeze({
  incentiveSeconds: 120,
  minimumDiscountBps: 50,
  maximumDiscountBps: 500,
  initialEmergencyRewardBps: 30,
  maximumEmergencyRewardBps: 100,
  emergencyMaintenanceFractionBps: 7_000,
  recoveryBufferBps: 200,
  solventFeeBps: 100,
  protocolFeeShareBps: 2_000,
  maximumPrincipalCoverageBps: 7_500,
  maximumDailyInsuranceDrawBps: 5_000,
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

/** Time increases incentives; it never grants emergency or full-close access. */
export function liquidationIncentives(input: LiquidationHealthInput, distressAgeSeconds: bigint) {
  amount(distressAgeSeconds, "distressAgeSeconds");
  const health = liquidationHealth(input);
  if (!health.eligible) throw new Error("Position is not liquidatable");
  const progress = BigInt(health.distressProgressBps);
  const elapsed = min(distressAgeSeconds, 120n);
  return {
    buyerDiscountBps: Number(50n + max(450n * progress / BPS, 450n * elapsed / 120n)),
    emergencyRewardBps: Number(max(100n * progress / BPS, 30n + 70n * elapsed / 120n)),
    emergencyAllowed: health.emergencyAllowed,
  };
}

export function liquidationDistressAge(episode: { active: boolean; startedAt: bigint }, now: bigint): bigint {
  if (!episode.active) return 0n;
  if (now < episode.startedAt) throw new RangeError("Clock precedes the committed distress episode");
  const age = now - episode.startedAt;
  amount(age, "distressAgeSeconds");
  return age;
}

/** P excludes hLP funding. H is current indexed hLP debt, including interest. */
export function liquidationInsuranceTarget(principal: bigint, hlpIndexedDebt: bigint): bigint {
  for (const [name, value] of Object.entries({ principal, hlpIndexedDebt })) {
    if (typeof value !== "bigint" || value < 0n || value > (1n << 128n) - 1n) throw new RangeError(`${name} must be an unsigned u128 bigint`);
  }
  const target = ceilDiv(principal, 20n) + ceilDiv(hlpIndexedDebt, 15n);
  amount(target, "target");
  return target;
}

/** Net fee atoms. Physical transfer-fee gross-ups are additional, separate costs. */
export function liquidationFeeAllocation(netDebtRepaid: bigint, insuranceBalance: bigint, target: bigint, surplusCap = U64_MAX) {
  for (const [name, value] of Object.entries({ netDebtRepaid, insuranceBalance, target, surplusCap })) amount(value, name);
  const total = min(netDebtRepaid / 100n, surplusCap);
  const protocol = total / 5n;
  const designated = total - protocol;
  const shortfall = max(0n, target - insuranceBalance);
  const insurance = target === 0n || shortfall === 0n ? 0n
    : insuranceBalance * 4n <= target * 3n ? min(designated, shortfall)
    : min(designated * 4n * shortfall / target, shortfall);
  return { total, protocol, insurance, lp: designated - insurance };
}

export function liquidationLossAllocation(principal: bigint, debt: bigint, recovery: bigint, insuranceNetCapacity: bigint, coverageBps = 7_500) {
  for (const [name, value] of Object.entries({ principal, debt, recovery, insuranceNetCapacity })) amount(value, name);
  if (principal > debt || !Number.isSafeInteger(coverageBps) || coverageBps < 0 || coverageBps > 7_500) throw new RangeError("Invalid principal or insurance coverage");
  const principalRepaid = min(principal, recovery);
  const gap = principal - principalRepaid;
  const insuranceCredit = min(gap * BigInt(coverageBps) / BPS, insuranceNetCapacity);
  const interestPaid = min(debt - principal, max(0n, recovery - principal));
  return { principalRepaid, interestPaid, insuranceCredit, principalWrittenOff: gap - insuranceCredit, interestCanceled: debt - principal - interestPaid, surplus: max(0n, recovery - debt) };
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
