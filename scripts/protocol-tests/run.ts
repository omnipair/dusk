import { ProtocolTestHarness } from "./harness.js";
import { BASELINE_SCENARIOS } from "./scenarios/baseline.js";
import { BOOTSTRAP_SCENARIOS } from "./scenarios/bootstrap.js";
import { COMPATIBILITY_SCENARIOS } from "./scenarios/compatibility.js";
import { GOVERNANCE_SCENARIOS } from "./scenarios/governance.js";
import { LEVERAGE_SCENARIOS } from "./scenarios/leverage.js";
import { LENDING_SCENARIOS } from "./scenarios/lending.js";
import { LIQUIDITY_SCENARIOS } from "./scenarios/liquidity.js";
import { LIQUIDATION_SCENARIOS } from "./scenarios/liquidation.js";
import {
  MARKET_BOUNDARY_SCENARIOS,
  POST_GOVERNANCE_MARKET_SCENARIOS,
} from "./scenarios/market_boundaries.js";
import { REFERRAL_SCENARIOS } from "./scenarios/referral.js";
import { SECURITY_SCENARIOS } from "./scenarios/security.js";
import { STRESS_SCENARIOS } from "./scenarios/stress.js";

async function main(): Promise<void> {
  const harness = new ProtocolTestHarness();
  await harness.initialize();
  console.log(`Dusk protocol client run: ${harness.id}`);
  console.log(`RPC: ${harness.config.rpcUrl}`);
  console.log(`Market: ${harness.config.market}`);
  const finalInvariant = BASELINE_SCENARIOS.find(
    (scenario) => scenario.id === "invariant.post-baseline-solvency"
  );
  const requiredSetupIds = new Set(["system.bootstrap-clean", "system.real-wallet-funding"]);
  const setupScenarios = BASELINE_SCENARIOS.filter((scenario) => requiredSetupIds.has(scenario.id));
  const remainingBaselineScenarios = BASELINE_SCENARIOS.filter(
    (scenario) => scenario !== finalInvariant && !requiredSetupIds.has(scenario.id)
  );
  const allScenarios = [
    ...setupScenarios,
    ...BOOTSTRAP_SCENARIOS,
    ...remainingBaselineScenarios,
    ...LIQUIDITY_SCENARIOS,
    ...MARKET_BOUNDARY_SCENARIOS,
    ...LENDING_SCENARIOS,
    ...LEVERAGE_SCENARIOS,
    ...LIQUIDATION_SCENARIOS,
    ...GOVERNANCE_SCENARIOS,
    ...POST_GOVERNANCE_MARKET_SCENARIOS,
    ...REFERRAL_SCENARIOS,
    ...SECURITY_SCENARIOS,
    ...COMPATIBILITY_SCENARIOS,
    ...STRESS_SCENARIOS,
    ...(finalInvariant ? [finalInvariant] : []),
  ];
  const requestedIds = new Set(
    (process.env.PROTOCOL_TEST_SCENARIOS ?? "")
      .split(",")
      .map((id) => id.trim())
      .filter(Boolean)
  );
  const scenarios = requestedIds.size === 0
    ? allScenarios
    : allScenarios.filter((scenario) => requestedIds.has(scenario.id) || requiredSetupIds.has(scenario.id));
  if (requestedIds.size > 0) {
    const selectedIds = new Set(scenarios.map((scenario) => scenario.id));
    const unknownIds = [...requestedIds].filter((id) => !selectedIds.has(id));
    if (unknownIds.length > 0) throw new Error(`Unknown or unimplemented scenarios: ${unknownIds.join(", ")}`);
    console.log(`Scenario filter: ${[...requestedIds].join(", ")}`);
  }
  const report = await harness.runScenarios(scenarios);
  console.log("\nProtocol client run complete");
  console.log(`Status: ${report.status}`);
  console.log(`Scenarios: ${report.summary.passed} passed, ${report.summary.failed} failed, ${report.summary.notRun} not run`);
  console.log(`Transactions: ${report.summary.transactionsSubmitted} confirmed`);
  console.log(`Expected failures: ${report.summary.expectedFailuresVerified} verified`);
  console.log(`Assertions: ${report.summary.assertionsPassed}/${report.summary.assertions}`);
  console.log(`Instruction execution: ${report.coverage.executed.length}/${report.coverage.total}`);
  console.log(`Report: ${harness.reportPath}`);
  if (report.status === "failed") process.exitCode = 1;
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
