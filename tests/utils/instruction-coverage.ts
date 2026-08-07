/**
 * Instruction smoke coverage tracking for LiteSVM tests.
 * Tracks which program instructions are exercised at least once.
 */

import { createHash } from "crypto";
import { PublicKey, Transaction, VersionedTransaction } from "@solana/web3.js";

type InstructionId = string;

const DUSK_PROGRAM_ID = new PublicKey(
  "358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv"
);

export const REQUIRED_SWAP_COMPUTE_SCENARIOS = [
  "cpmm_same_slot",
  "cpmm_advanced_slot",
  "cpmm_active_debt",
  "concentrated_centered",
  "concentrated_transition",
  "concentrated_tail",
  "dynamic_fee_divergence_stress",
  "dynamic_fee_volatility_stress",
  "retained_surcharge",
  "controller_due_ramp",
  "controller_due_recenter",
  "hlp_active",
  "hlp_residual_correction",
  "token_2022_swap",
] as const;

export type SwapComputeScenario = (typeof REQUIRED_SWAP_COMPUTE_SCENARIOS)[number];

type ComputeScenarioBaseline = {
  measuredMaximum: bigint;
  ceiling: bigint;
};

type ComputeScenarioDetail = {
  count: number;
  total: bigint;
  max: bigint;
};

type MeasuredSwapTransaction = {
  transaction: Transaction | VersionedTransaction;
  computeUnits: bigint;
};

/**
 * The ordinary legacy-SPL path is an architectural acceptance rule, not a
 * measured-regression allowance. It must remain strictly below 100k CU.
 *
 * The remaining baselines are populated only from a clean, fully successful
 * deterministic LiteSVM run of the finished SBF binary. Their ceilings are
 * exactly ceil(measured maximum * 1.05). Keeping the values together makes a
 * baseline update reviewable instead of silently blessing the current run.
 */
export const ORDINARY_SWAP_COMPUTE_UNIT_LIMIT = 100_000n;

const COMPUTE_SCENARIO_BASELINES: Partial<
  Record<SwapComputeScenario, ComputeScenarioBaseline>
> = {
  cpmm_same_slot: { measuredMaximum: 56_215n, ceiling: 59_026n },
  cpmm_advanced_slot: { measuredMaximum: 89_304n, ceiling: 93_770n },
  cpmm_active_debt: { measuredMaximum: 97_982n, ceiling: 102_882n },
  concentrated_centered: { measuredMaximum: 188_883n, ceiling: 198_328n },
  concentrated_transition: { measuredMaximum: 274_752n, ceiling: 288_490n },
  concentrated_tail: { measuredMaximum: 107_976n, ceiling: 113_375n },
  dynamic_fee_divergence_stress: {
    measuredMaximum: 356_950n,
    ceiling: 374_798n,
  },
  dynamic_fee_volatility_stress: {
    measuredMaximum: 105_407n,
    ceiling: 110_678n,
  },
  retained_surcharge: { measuredMaximum: 474_610n, ceiling: 498_341n },
  controller_due_ramp: { measuredMaximum: 489_599n, ceiling: 514_079n },
  controller_due_recenter: {
    measuredMaximum: 403_274n,
    ceiling: 423_438n,
  },
  hlp_active: { measuredMaximum: 105_701n, ceiling: 110_987n },
  hlp_residual_correction: { measuredMaximum: 164_258n, ceiling: 172_471n },
  token_2022_swap: { measuredMaximum: 65_448n, ceiling: 68_721n },
};

Object.entries(COMPUTE_SCENARIO_BASELINES).forEach(([scenario, baseline]) => {
  const exactFivePercentCeiling =
    (baseline.measuredMaximum * 105n + 99n) / 100n;
  if (baseline.ceiling !== exactFivePercentCeiling) {
    throw new Error(
      `${scenario} CU ceiling must equal ceil(measured maximum * 1.05): expected ${exactFivePercentCeiling}, got ${baseline.ceiling}`
    );
  }
});

const testedInstructions = new Set<InstructionId>();
const instructionDetails = new Map<InstructionId, { count: number; tests: string[] }>();
const computeUnitDetails = new Map<
  InstructionId,
  { count: number; total: bigint; max: bigint }
>();
const computeScenarioDetails = new Map<SwapComputeScenario, ComputeScenarioDetail>();
let externalTransferHookCompute: ComputeScenarioDetail | undefined;
let measuredTransactionCount = 0;
let measuredTransactionTotal = 0n;
let measuredTransactionMax = 0n;
let lastPrintedReportSignature: string | undefined;

export const LITESVM_COMPUTE_UNIT_LIMIT = BigInt(
  // Keep a 50k-CU repository release guard below Solana's 1.4M transaction
  // ceiling. The exact retained-surcharge concentrated path is the measured
  // high-water mark; bounded wide-domain u128 fee paths are gated separately.
  process.env.DUSK_TEST_COMPUTE_UNIT_LIMIT ?? "1350000"
);

const DUSK_INSTRUCTIONS = [
  "initFutarchyAuthority",
  "updateFutarchyAuthority",
  "updateProtocolRevenue",
  "updateRevenueRecipients",
  "updateProtocolAuctionConfig",
  "updateProtocolAuctionRecipients",
  "updateProtocolAuctionRoute",
  "setGlobalReduceOnly",
  "configureReferralPartner",
  "initializeReferralAccrual",
  "setReferralRecipient",
  "claimReferralInterest",
  "settleProtocolAuction",
  "initializeMarket",
  "initializeLpMetadata",
  "initializeYieldAccounts",
  "initializeLpTransferHook",
  "setMarketReduceOnly",
  "createParameterProposal",
  "supportParameterProposal",
  "queueParameterProposal",
  "executeParameterProposal",
  "withdrawParameterSupport",
  "addLiquidity",
  "removeLiquidity",
  "setYieldRecipient",
  "claimYield",
  "swap",
  "depositCollateral",
  "withdrawCollateral",
  "borrow",
  "repay",
  "openLeverage",
  "closeLeverage",
  "delegatedCloseLeverage",
  "increaseLeverage",
  "decreaseLeverage",
  "addLeverageMargin",
  "removeLeverageMargin",
  "liquidateLeverage",
  "createLeverageDelegation",
  "updateLeverageDelegation",
  "closeLeverageDelegation",
  "triggerLiquidationAuction",
  "bidLiquidationAuction",
  "settleLiquidationAuctionFloor",
  "previewMarket",
  "previewAddLiquidity",
  "previewSwap",
  "previewBorrowCapacity",
  "previewBorrowPosition",
  "depositSingleSided",
  "withdrawSingleSided",
];

const ALL_INSTRUCTIONS = DUSK_INSTRUCTIONS;

function anchorInstructionDiscriminator(instructionName: string): string {
  const snakeCase = instructionName.replace(
    /[A-Z]/g,
    (letter) => `_${letter.toLowerCase()}`
  );
  return createHash("sha256")
    .update(`global:${snakeCase}`)
    .digest()
    .subarray(0, 8)
    .toString("hex");
}

const INSTRUCTION_BY_DISCRIMINATOR = new Map(
  ALL_INSTRUCTIONS.map((instructionName) => [
    anchorInstructionDiscriminator(instructionName),
    instructionName,
  ])
);

function instructionLabel(id: InstructionId): string {
  return id;
}

function track(instructionName: string, testName?: string) {
  const id = instructionName;
  testedInstructions.add(id);

  const detail = instructionDetails.get(id) || { count: 0, tests: [] };
  detail.count++;
  if (testName && !detail.tests.includes(testName)) {
    detail.tests.push(testName);
  }
  instructionDetails.set(id, detail);

  console.log(`  ✓ Tested: ${instructionLabel(id)}`);
}

function coverageDataFor(instructions: InstructionId[]) {
  const coveredInstructions = instructions.filter((ix) => testedInstructions.has(ix));
  const untestedInstructions = instructions.filter((ix) => !testedInstructions.has(ix));
  const covered = coveredInstructions.length;
  const total = instructions.length;
  const percentage = total === 0 ? "100.00" : ((covered / total) * 100).toFixed(2);

  return {
    covered,
    total,
    percentage,
    testedInstructions: coveredInstructions,
    untestedInstructions,
  };
}

function reportSignature(): string {
  return Array.from(testedInstructions).sort().join("|");
}

function printCoverageSection(title: string, instructions: InstructionId[]) {
  const data = coverageDataFor(instructions);
  const untestedInstructions = data.untestedInstructions;

  console.log(`\n${title}`);
  console.log(
    `Instructions Exercised: ${data.covered}/${data.total} (${data.percentage}%)\n`
  );

  data.testedInstructions.forEach((ix) => {
    const detail = instructionDetails.get(ix);
    const testCount = detail?.tests.length || 0;
    console.log(`  ✓ ${instructionLabel(ix).padEnd(28)} [${testCount} test(s)]`);
    if (detail?.tests.length) {
      detail.tests.forEach((test) => {
        console.log(`    └─ ${test}`);
      });
    }
  });

  if (untestedInstructions.length > 0) {
    console.log(`\nUnexercised Instructions: ${untestedInstructions.length}/${data.total}\n`);
    untestedInstructions.forEach((ix) => {
      console.log(`  ✗ ${instructionLabel(ix)}`);
    });
  }

}

/**
 * Track that an instruction was tested
 * @param instructionName Name of the instruction tested
 * @param testName Name of the test that used it
 */
export function trackInstruction(instructionName: string, testName?: string) {
  track(instructionName, testName);
}

/**
 * Track that a Dusk instruction was tested.
 */
export function trackV2Instruction(instructionName: string, testName?: string) {
  track(instructionName, testName);
}

/**
 * Attribute LiteSVM's measured transaction cost to every top-level Dusk
 * instruction in the transaction. The suite's configured runtime limit
 * remains the hard assertion; this telemetry makes actual headroom visible.
 */
export function recordTransactionComputeUnits(
  transaction: Transaction | VersionedTransaction,
  computeUnits: bigint
) {
  measuredTransactionCount++;
  measuredTransactionTotal += computeUnits;
  measuredTransactionMax =
    computeUnits > measuredTransactionMax ? computeUnits : measuredTransactionMax;

  const matched = new Set(topLevelDuskInstructions(transaction));

  matched.forEach((instructionName) => {
    const detail = computeUnitDetails.get(instructionName) || {
      count: 0,
      total: 0n,
      max: 0n,
    };
    detail.count++;
    detail.total += computeUnits;
    detail.max = computeUnits > detail.max ? computeUnits : detail.max;
    computeUnitDetails.set(instructionName, detail);
  });
}

/**
 * Attribute one explicitly returned successful LiteSVM transaction to a
 * deterministic swap-path scenario and enforce its checked-in CI guard.
 */
export function recordSwapComputeScenario(
  scenario: SwapComputeScenario,
  measurement: MeasuredSwapTransaction
) {
  const duskInstructions = topLevelDuskInstructions(measurement.transaction);
  if (duskInstructions.length !== 1 || duskInstructions[0] !== "swap") {
    throw new Error(
      `${scenario} must measure exactly one top-level Dusk swap; found ${duskInstructions.join(", ") || "none"}`
    );
  }
  const { computeUnits } = measurement;

  const detail = computeScenarioDetails.get(scenario) ?? {
    count: 0,
    total: 0n,
    max: 0n,
  };
  detail.count++;
  detail.total += computeUnits;
  detail.max = computeUnits > detail.max ? computeUnits : detail.max;
  computeScenarioDetails.set(scenario, detail);

  if (
    scenario === "cpmm_same_slot" &&
    computeUnits >= ORDINARY_SWAP_COMPUTE_UNIT_LIMIT
  ) {
    throw new Error(
      `${scenario} consumed ${computeUnits.toLocaleString()} CU; ordinary legacy-SPL swaps must remain below ${ORDINARY_SWAP_COMPUTE_UNIT_LIMIT.toLocaleString()} CU`
    );
  }

  const baseline = COMPUTE_SCENARIO_BASELINES[scenario];
  if (baseline && computeUnits > baseline.ceiling) {
    throw new Error(
      `${scenario} consumed ${computeUnits.toLocaleString()} CU; checked-in ceiling is ${baseline.ceiling.toLocaleString()} CU (measured maximum ${baseline.measuredMaximum.toLocaleString()} CU + 5%)`
    );
  }
}

function topLevelDuskInstructions(
  transaction: Transaction | VersionedTransaction
): InstructionId[] {
  const instructionData = transaction instanceof Transaction
    ? transaction.instructions
        .filter((instruction) => instruction.programId.equals(DUSK_PROGRAM_ID))
        .map((instruction) => instruction.data)
    : transaction.message.compiledInstructions.flatMap((instruction) => {
        const programId = transaction.message.staticAccountKeys[instruction.programIdIndex];
        return programId?.equals(DUSK_PROGRAM_ID) ? [instruction.data] : [];
      });
  return instructionData.flatMap((data) => {
    if (data.length < 8) {
      return [];
    }
    const discriminator = Buffer.from(data).subarray(0, 8).toString("hex");
    const instructionName = INSTRUCTION_BY_DISCRIMINATOR.get(discriminator);
    return instructionName ? [instructionName] : [];
  });
}

/** Record the full Token-2022 + hook transaction separately from swap CU. */
export function recordExternalTransferHookComputeUnits(computeUnits: bigint | undefined) {
  if (computeUnits === undefined) {
    throw new Error("LiteSVM did not expose compute units for the transfer-hook transaction");
  }
  const detail = externalTransferHookCompute ?? { count: 0, total: 0n, max: 0n };
  detail.count++;
  detail.total += computeUnits;
  detail.max = computeUnits > detail.max ? computeUnits : detail.max;
  externalTransferHookCompute = detail;
}

export function assertRequiredSwapComputeScenarios() {
  const unmeasuredInstructions = ALL_INSTRUCTIONS.filter(
    (instruction) => !computeUnitDetails.has(instruction)
  );
  if (unmeasuredInstructions.length > 0) {
    throw new Error(
      `Missing successful CU measurements for Dusk instructions: ${unmeasuredInstructions.join(", ")}`
    );
  }

  const missing = REQUIRED_SWAP_COMPUTE_SCENARIOS.filter(
    (scenario) => !computeScenarioDetails.has(scenario)
  );
  if (missing.length > 0) {
    throw new Error(`Missing deterministic swap CU scenarios: ${missing.join(", ")}`);
  }

  const missingBaselines = REQUIRED_SWAP_COMPUTE_SCENARIOS.filter(
    (scenario) => COMPUTE_SCENARIO_BASELINES[scenario] === undefined
  );
  if (missingBaselines.length > 0) {
    throw new Error(
      `Missing finished-binary CU baselines: ${missingBaselines.join(", ")}. Populate only from a fully successful deterministic LiteSVM run.`
    );
  }
  if (!externalTransferHookCompute) {
    throw new Error(
      "Missing external Token-2022 transfer-hook transaction CU measurement"
    );
  }
}

function printComputeScenarioReport() {
  console.log("\nDeterministic Swap-Path Compute Scenarios");
  REQUIRED_SWAP_COMPUTE_SCENARIOS.forEach((scenario) => {
    const detail = computeScenarioDetails.get(scenario);
    const baseline = COMPUTE_SCENARIO_BASELINES[scenario];
    if (!detail) {
      console.log(`  ✗ ${scenario.padEnd(32)} not measured`);
      return;
    }
    const average = detail.total / BigInt(detail.count);
    const guard =
      scenario === "cpmm_same_slot"
        ? `< ${ORDINARY_SWAP_COMPUTE_UNIT_LIMIT.toLocaleString()}`
        : baseline
          ? `≤ ${baseline.ceiling.toLocaleString()}`
          : "baseline pending clean finished-binary run";
    console.log(
      `  ✓ ${scenario.padEnd(32)} max ${detail.max
        .toLocaleString()
        .padStart(9)} | avg ${average
        .toLocaleString()
        .padStart(9)} | n=${detail.count.toString().padStart(2)} | guard ${guard}`
    );
  });

  if (
    REQUIRED_SWAP_COMPUTE_SCENARIOS.every((scenario) =>
      computeScenarioDetails.has(scenario)
    )
  ) {
    console.log("\nFinished-binary baseline candidates (accept only if the full suite passed):");
    REQUIRED_SWAP_COMPUTE_SCENARIOS.forEach((scenario) => {
      const measuredMaximum = computeScenarioDetails.get(scenario)!.max;
      const ceiling = (measuredMaximum * 105n + 99n) / 100n;
      console.log(
        `  ${scenario}: { measuredMaximum: ${measuredMaximum}n, ceiling: ${ceiling}n },`
      );
    });
  }

  if (externalTransferHookCompute) {
    const average =
      externalTransferHookCompute.total / BigInt(externalTransferHookCompute.count);
    console.log(
      `\nExternal Token-2022 transfer-hook transaction: max ${externalTransferHookCompute.max.toLocaleString()} CU | avg ${average.toLocaleString()} CU | n=${externalTransferHookCompute.count}`
    );
  } else {
    console.log("\nExternal Token-2022 transfer-hook transaction: not measured");
  }
}

function printComputeUnitReport() {
  if (measuredTransactionCount === 0) {
    console.log("\nLiteSVM Compute Units: no successful transaction metadata recorded");
    return;
  }

  const average = measuredTransactionTotal / BigInt(measuredTransactionCount);
  console.log("\nLiteSVM Compute-Unit Report");
  console.log(
    `Successful transactions: ${measuredTransactionCount} | average: ${average.toLocaleString()} CU | maximum: ${measuredTransactionMax.toLocaleString()} CU`
  );
  console.log(
    `Top-level Dusk instruction maxima (${LITESVM_COMPUTE_UNIT_LIMIT.toLocaleString()}-CU test limit):`
  );

  Array.from(computeUnitDetails.entries())
    .sort((left, right) =>
      left[1].max === right[1].max
        ? left[0].localeCompare(right[0])
        : left[1].max > right[1].max
          ? -1
          : 1
    )
    .forEach(([instructionName, detail]) => {
      const averageForInstruction = detail.total / BigInt(detail.count);
      const headroomBps =
        10_000n - (detail.max * 10_000n) / LITESVM_COMPUTE_UNIT_LIMIT;
      console.log(
        `  ${instructionLabel(instructionName).padEnd(28)} max ${detail.max
          .toLocaleString()
          .padStart(7)} | avg ${averageForInstruction
          .toLocaleString()
          .padStart(7)} | n=${detail.count
          .toString()
          .padStart(2)} | headroom ${(Number(headroomBps) / 100).toFixed(2)}%`
      );
    });
}

/**
 * Get the coverage report
 */
export function getCoverageReport() {
  const aggregate = coverageDataFor(ALL_INSTRUCTIONS);
  const signature = reportSignature();

  if (signature === lastPrintedReportSignature) {
    return {
      covered: aggregate.covered,
      total: aggregate.total,
      percentage: parseFloat(aggregate.percentage),
      testedInstructions: aggregate.testedInstructions.map(instructionLabel),
      untestedInstructions: aggregate.untestedInstructions.map(instructionLabel),
    };
  }
  lastPrintedReportSignature = signature;
  
  console.log("\n" + "═".repeat(70));
  console.log("📊 INSTRUCTION SMOKE COVERAGE REPORT");
  console.log("═".repeat(70));
  console.log(
    "This tracks whether each instruction is exercised by at least one LiteSVM test."
  );
  console.log(
    "It is not statement, branch, invariant, or full behavioral coverage."
  );

  printCoverageSection("Dusk Instruction Smoke Coverage", ALL_INSTRUCTIONS);
  printComputeUnitReport();
  printComputeScenarioReport();
  
  console.log("\n" + "═".repeat(70));
  console.log(
    `Aggregate Smoke Coverage: ${aggregate.percentage}% | Instructions Exercised: ${aggregate.covered}/${aggregate.total}`
  );
  console.log("═".repeat(70) + "\n");
  
  return {
    covered: aggregate.covered,
    total: aggregate.total,
    percentage: parseFloat(aggregate.percentage),
    testedInstructions: aggregate.testedInstructions.map(instructionLabel),
    untestedInstructions: aggregate.untestedInstructions.map(instructionLabel),
  };
}

/**
 * Reset coverage tracking (for new test suite)
 */
export function resetCoverage() {
  testedInstructions.clear();
  instructionDetails.clear();
  computeUnitDetails.clear();
  computeScenarioDetails.clear();
  externalTransferHookCompute = undefined;
  measuredTransactionCount = 0;
  measuredTransactionTotal = 0n;
  measuredTransactionMax = 0n;
  lastPrintedReportSignature = undefined;
}

/**
 * Get current coverage as object
 */
export function getCoverageData() {
  const data = coverageDataFor(ALL_INSTRUCTIONS);

  return {
    covered: data.covered,
    total: data.total,
    percentage: data.percentage,
    testedInstructions: data.testedInstructions.map(instructionLabel),
    untestedInstructions: data.untestedInstructions.map(instructionLabel),
  };
}
