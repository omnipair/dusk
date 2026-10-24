import assert from "node:assert/strict";
import { test } from "node:test";
import { getStoredLeverageMargins } from "../dist/index.js";

const position = (collateral, retained) => ({
  collateralAmount: BigInt(collateral),
  marginTerms: {
    referenceCollateral: 50_000n,
    maintenanceBoundariesBps: [500, 1500],
    maintenanceRatesBps: [700, 1200, 1700],
    entryBufferBps: 300,
    admissionEquityCollateralNad: BigInt(retained) * 1_000_000_000n,
  },
});

test("stored margin display matches Rust entry and proportional-close examples", () => {
  const first = getStoredLeverageMargins(position(15_000, 2500));
  const later = getStoredLeverageMargins(position(15_000, 3000));
  assert.equal(first.initialMarginBps, 1667);
  assert.equal(later.initialMarginBps, 2000);
  assert.equal(first.maintenanceMarginBps, 1367);
  assert.equal(later.maintenanceMarginBps, 1367);
  const partial = getStoredLeverageMargins(position(5000, 1000));
  assert.equal(partial.initialMarginBps, 2000);
  assert.equal(partial.maintenanceMarginBps, 950);
});

test("invalid and zero margin references fail explicitly", () => {
  const invalid = position(15_000, 3000);
  invalid.marginTerms.referenceCollateral = 0n;
  assert.throws(() => getStoredLeverageMargins(invalid), /Invalid stored/);
  assert.throws(() => getStoredLeverageMargins(position(0, 0)), /Invalid stored/);
  assert.throws(() => getStoredLeverageMargins(position(15_000, 15_000)), /no leverage capacity/);
  assert.equal(getStoredLeverageMargins(position(1, 0)).maintenanceMarginBps, 700);
});
