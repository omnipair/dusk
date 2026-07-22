/**
 * Instruction smoke coverage tracking for LiteSVM tests.
 * Tracks which program instructions are exercised at least once.
 */

type InstructionId = string;

const testedInstructions = new Set<InstructionId>();
const instructionDetails = new Map<InstructionId, { count: number; tests: string[] }>();
const skippedInstructions = new Map<InstructionId, string>();
let lastPrintedReportSignature: string | undefined;

const DUSK_INSTRUCTIONS = [
  "initFutarchyAuthority",
  "updateFutarchyAuthority",
  "updateProtocolRevenue",
  "updateRevenueRecipients",
  "updateProtocolAuctionConfig",
  "updateProtocolAuctionRecipients",
  "setGlobalReduceOnly",
  "configureReferral",
  "initializeReferralAccrual",
  "setReferralRecipient",
  "claimReferralInterest",
  "settleProtocolAuction",
  "initialize",
  "initializeLpMetadata",
  "updateConfig",
  "setReduceOnly",
  "setOperator",
  "setManager",
  "claimManagerFees",
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
  "settleLiquidationAuctionAmm",
  "previewMarket",
  "previewAddLiquidity",
  "previewSwap",
  "previewBorrowCapacity",
  "previewBorrowPosition",
  "depositSingleSided",
  "withdrawSingleSided",
];

const ALL_INSTRUCTIONS = DUSK_INSTRUCTIONS;

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
  const tested = Array.from(testedInstructions).sort().join("|");
  const skipped = Array.from(skippedInstructions.entries())
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([instruction, reason]) => `${instruction}:${reason}`)
    .join("|");
  return `${tested}::${skipped}`;
}

function printCoverageSection(title: string, instructions: InstructionId[]) {
  const data = coverageDataFor(instructions);
  const skippedUntested = data.untestedInstructions.filter((ix) => skippedInstructions.has(ix));
  const untestedInstructions = data.untestedInstructions.filter(
    (ix) => !skippedInstructions.has(ix)
  );

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

  if (skippedUntested.length > 0) {
    console.log(`\nKnown Skips: ${skippedUntested.length}/${data.total}\n`);
    skippedUntested.forEach((ix) => {
      console.log(`  - ${instructionLabel(ix)}: ${skippedInstructions.get(ix)}`);
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
 * Record an intentionally skipped Dusk instruction smoke path.
 */
export function skipV2Instruction(instructionName: string, reason: string) {
  skippedInstructions.set(instructionName, reason);
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
  skippedInstructions.clear();
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
