import type { LeveragePosition } from "./type-aliases.js";

const BPS = 10_000n;
const NAD = 1_000_000_000n;
const U64_MAX = (1n << 64n) - 1n;
const ceilDiv = (a: bigint, b: bigint) => (a + b - 1n) / b;
const min = (a: bigint, b: bigint) => a < b ? a : b;

/** Read the position's stored requirements without repricing from live crowding.
 * These rates are not actual equity, a liquidation price, or an executable
 * opening/increase quote. Simulate the intended transaction for new risk.
 */
export function getStoredLeverageMargins(
  position: Pick<LeveragePosition, "collateralAmount" | "marginTerms">
) {
  const collateral = BigInt(position.collateralAmount.toString());
  const terms = position.marginTerms;
  const reference = BigInt(terms.referenceCollateral.toString());
  const retained = BigInt(terms.admissionEquityCollateralNad.toString());
  const [firstBps, secondBps] = terms.maintenanceBoundariesBps;
  const [low, middle, high] = terms.maintenanceRatesBps;
  const buffer = terms.entryBufferBps;
  if (collateral <= 0n || collateral > U64_MAX || reference <= 0n || reference > U64_MAX ||
      retained < 0n || retained >= (1n << 128n) ||
      ![firstBps, secondBps, low, middle, high, buffer].every(Number.isSafeInteger) ||
      firstBps <= 0 || firstBps >= secondBps || secondBps > 65_535 ||
      low <= 0 || low > middle || middle > high || buffer < 0 || high + buffer >= 10_000) {
    throw new Error("Invalid stored leverage margin terms");
  }
  const amount = collateral * BPS;
  const first = reference * BigInt(firstBps);
  const second = reference * BigInt(secondBps);
  const aboveFirst = amount > first ? amount - first : 0n;
  const aboveSecond = amount > second ? amount - second : 0n;
  const weighted = min(amount, first) * BigInt(low)
    + min(aboveFirst, second - first) * BigInt(middle)
    + aboveSecond * BigInt(high);
  const maintenanceMarginBps = Number(ceilDiv(weighted, amount));
  const retainedBps = ceilDiv(retained * BPS, collateral * NAD);
  if (retainedBps >= BPS) throw new Error("Stored admission requirement leaves no leverage capacity");
  const initialMarginBps = Math.max(maintenanceMarginBps + buffer, Number(retainedBps));
  return {
    initialMarginBps,
    maintenanceMarginBps,
    referenceCollateral: reference,
    admissionEquityCollateralNad: retained,
  };
}
