import assert from "node:assert/strict";
import { test } from "node:test";
import { RISK_RATE_DEFAULTS, riskCoverageSignalNad } from "../dist/risk-pricing.js";
import { deriveIncomeEpochAddress, deriveJuniorMintAddress, deriveJuniorExitAddress } from "../dist/constants.js";
import { PublicKey } from "@solana/web3.js";

test("coverage signal distinguishes capital conversion, borrowing and senior cash movement", () => {
  const signal = (junior, debt, cash) => riskCoverageSignalNad(BigInt(junior), BigInt(debt), BigInt(debt), BigInt(cash));
  assert.equal(signal(20, 80, 20), 50_000_000n);
  assert.equal(signal(30, 80, 20), 75_000_000n);
  assert.equal(signal(10, 80, 20), 25_000_000n);
  assert.equal(signal(20, 90, 10), 22_222_222n);
  assert.equal(signal(20, 70, 30), 85_714_285n);
  assert.equal(signal(20, 80, 40), 83_350_000n);
  assert.equal(signal(20, 80, 10), 27_800_000n);
});

test("both assets share one junior budget while using their own cash constraint", () => {
  assert.equal(riskCoverageSignalNad(20n, 100n, 80n, 20n), 40_000_000n);
  assert.equal(riskCoverageSignalNad(20n, 100n, 20n, 80n), 160_000_000n);
  assert.equal(riskCoverageSignalNad(0n, 100n, 80n, 20n), 0n);
  assert.equal(riskCoverageSignalNad(20n, 100n, 80n, 0n), 0n);
  assert.equal(riskCoverageSignalNad(20n, 100n, 0n, 100n), null);
  assert.throws(() => riskCoverageSignalNad(20n, 0n, 1n, 1n));
  assert.throws(() => riskCoverageSignalNad(-1n, 100n, 80n, 20n));
});

test("creation defaults retain the approved half-life and explicit uncapped rate", () => {
  assert.equal(RISK_RATE_DEFAULTS.halfLifeMs, 1_209_600_000);
  assert.equal(RISK_RATE_DEFAULTS.lowerSignalNad, 50_000_000);
  assert.equal(RISK_RATE_DEFAULTS.upperSignalNad, 62_500_000);
  assert.equal(RISK_RATE_DEFAULTS.initialAprNad, 40_000_000);
  assert.equal(RISK_RATE_DEFAULTS.minimumAprNad, 20_000_000);
  assert.equal(RISK_RATE_DEFAULTS.maximumAprNad, 0);
  assert.equal(RISK_RATE_DEFAULTS.borrowUtilizationLimitBps, 8_000);
});

test("junior generations and archived income sources cannot share addresses", () => {
  const market = new PublicKey(new Uint8Array(32).fill(21));
  const owner = new PublicKey(new Uint8Array(32).fill(22));
  const [first] = deriveJuniorMintAddress(market, 0n);
  const [second] = deriveJuniorMintAddress(market, 1n);
  assert(!first.equals(second));
  assert(!deriveJuniorExitAddress(market, owner, first)[0].equals(deriveJuniorExitAddress(market, owner, second)[0]));
  const addresses = [0, 1, 2, 3].flatMap(book => [0n, 1n].map(epoch => deriveIncomeEpochAddress(market, book, epoch)[0].toBase58()));
  assert.equal(new Set(addresses).size, 8);
  assert.throws(() => deriveIncomeEpochAddress(market, 4, 0n));
});
