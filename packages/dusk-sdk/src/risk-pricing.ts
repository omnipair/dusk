/** Creation defaults. Governance may replace each market's controller. */
export const RISK_RATE_DEFAULTS = Object.freeze({
  lowerSignalNad: 50_000_000,
  upperSignalNad: 62_500_000,
  halfLifeMs: 14 * 24 * 60 * 60 * 1_000,
  initialAprNad: 40_000_000,
  minimumAprNad: 20_000_000,
  maximumAprNad: 0,
  borrowUtilizationLimitBps: 8_000,
});

/** Values use one common numeraire. Returns null when this asset has no debt.
 * This is the controller's coverage/cash signal, not a default probability. */
export function riskCoverageSignalNad(
  juniorFundedValue: bigint,
  totalCoveredDebtValue: bigint,
  assetDebt: bigint,
  assetCash: bigint
): bigint | null {
  if ([juniorFundedValue, totalCoveredDebtValue, assetDebt, assetCash].some(v => v < 0n)) {
    throw new Error("Coverage inputs cannot be negative");
  }
  if (assetDebt === 0n) return null;
  if (totalCoveredDebtValue === 0n) throw new Error("Covered debt value is zero for an indebted asset");
  // Match the native bps utilization input, including its floor rounding.
  const utilizationBps = assetDebt * 10_000n / (assetDebt + assetCash);
  return juniorFundedValue * ((10_000n - utilizationBps) * 100_000n) / totalCoveredDebtValue;
}
