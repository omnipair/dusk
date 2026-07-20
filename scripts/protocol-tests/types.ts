export type ScenarioKind =
  | "happy-path"
  | "rejection"
  | "boundary"
  | "state-machine"
  | "stress"
  | "invariant";

export type ScenarioStatus = "passed" | "failed" | "skipped" | "not-run";

export interface ScenarioCatalogEntry {
  id: string;
  feature: string;
  title: string;
  purpose: string;
  kind: ScenarioKind;
  instructions: string[];
  tags: string[];
}

export interface SimulationEvidence {
  err: unknown | null;
  unitsConsumed: number | null;
  logs: string[];
  cpiEventData?: string[];
  returnData: {
    programId: string;
    data: [string, string];
  } | null;
}

export interface TransactionEvidence {
  type: "transaction";
  label: string;
  wallet: string;
  endpoint: string;
  expected: "success" | "failure";
  status: "passed" | "failed";
  submitted: boolean;
  durationMs: number;
  signature: string | null;
  slot: number | null;
  instructions: string[];
  simulation: SimulationEvidence;
  errorCode: string | null;
  error: string | null;
}

export interface AssertionEvidence {
  type: "assertion";
  label: string;
  status: "passed" | "failed";
  expected: unknown;
  actual: unknown;
  detail: string | null;
}

export interface ObservationEvidence {
  type: "observation";
  label: string;
  value: unknown;
}

export type ScenarioEvidence = TransactionEvidence | AssertionEvidence | ObservationEvidence;

export interface ScenarioResult {
  id: string;
  feature: string;
  title: string;
  purpose: string;
  kind: ScenarioKind;
  instructions: string[];
  tags: string[];
  status: ScenarioStatus;
  startedAt: string | null;
  finishedAt: string | null;
  durationMs: number | null;
  evidence: ScenarioEvidence[];
  error: string | null;
}

export interface InstructionCoverage {
  total: number;
  catalogTargeted: string[];
  catalogMissing: string[];
  executed: string[];
  executionMissing: string[];
}

export interface RunSummary {
  total: number;
  passed: number;
  failed: number;
  skipped: number;
  notRun: number;
  transactionsSubmitted: number;
  expectedFailuresVerified: number;
  assertions: number;
  assertionsPassed: number;
}

export interface ProtocolTestRun {
  schemaVersion: 1;
  runId: string;
  status: "running" | "passed" | "failed";
  startedAt: string;
  finishedAt: string | null;
  durationMs: number | null;
  gitRevision: string;
  rpcUrl: string;
  apiUrl: string;
  programId: string;
  market: string;
  wallets: Record<string, string>;
  idlInstructions: string[];
  coverage: InstructionCoverage;
  summary: RunSummary;
  scenarios: ScenarioResult[];
}
