import { PublicKey } from "@solana/web3.js";
import { address, type AddressLike } from "./address.js";
import { DUSK_PROGRAM_ID, deriveCancellationBallotAddress, deriveRemovalBallotAddress } from "./constants.js";
import { governanceIntegerBN, type GovernanceIntegerLike } from "./governance.js";

export const MANAGER_POLICY_VERSION = 1;
export const MANAGER_PENDING_LIMIT = 1;
export const MANAGER_ANNOUNCEMENT_COOLDOWN_SECONDS = 3 * 24 * 60 * 60;
// These match the junior-exit scheduled-slot clock (200ms nominal slots).
export const JUNIOR_MAX_NOTICE_SLOTS = 4_320_000;
export const MANAGER_NOTICE_SLOTS = 4_752_000;
export const MANAGER_EXECUTION_WINDOW_SLOTS = 3_024_000;
export type ProposalClockUnit = "unixSeconds" | "slots";
/** Select the unit for queuedAt, executeAfter and executionDeadline. */
export function proposalClockUnit(origin: {lp?: unknown; manager?: unknown}): ProposalClockUnit {
  if (origin.manager !== undefined) return "slots";
  if (origin.lp !== undefined) return "unixSeconds";
  throw new Error("unknown proposal origin");
}
export const RECOVERY_BALLOT_WINDOW_SECONDS = 7 * 24 * 60 * 60;
export type RecoveryTargetInput =
  | {kind: "cancelProposal"; proposal: AddressLike; digest: Uint8Array | readonly number[]}
  | {kind: "removeManager"; epoch: GovernanceIntegerLike};

export function anchorRecoveryTarget(target: RecoveryTargetInput): Record<string, unknown> {
  if (target.kind === "removeManager") return {removeManager: {epoch: governanceIntegerBN(target.epoch, "epoch")}};
  const digest = checkedDigest(target.digest);
  return {cancelProposal: {proposal: address(target.proposal), digest: Array.from(digest)}};
}

export async function deriveRecoveryBallotAddress(market: AddressLike, target: RecoveryTargetInput, programId: PublicKey = DUSK_PROGRAM_ID): Promise<[PublicKey, number]> {
  if (target.kind === "removeManager") {
    governanceIntegerBN(target.epoch, "epoch");
    return deriveRemovalBallotAddress(address(market), programId);
  }
  checkedDigest(target.digest);
  return deriveCancellationBallotAddress(address(market), address(target.proposal), programId);
}

export function deriveRecoveryVoteAddress(ballot: AddressLike, round: GovernanceIntegerLike, source: AddressLike, programId: PublicKey = DUSK_PROGRAM_ID): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from("recovery_vote"), address(ballot).toBuffer(), governanceIntegerBN(round, "round").toArrayLike(Buffer, "le", 8), address(source).toBuffer()], programId);
}

export function recoveryTargetFromAnchor(value: unknown): RecoveryTargetInput {
  const target = value as {removeManager?: {epoch: GovernanceIntegerLike}; cancelProposal?: {proposal: AddressLike; digest: number[]}};
  if (target.removeManager) return {kind: "removeManager", epoch: target.removeManager.epoch};
  if (target.cancelProposal) return {kind: "cancelProposal", ...target.cancelProposal};
  throw new Error("unknown recovery target");
}

export const PROPOSAL_STATUS_NAMES = ["collecting", "queued", "executed", "cancelled", "expired", "stale"] as const;
export type ProposalOutcome = typeof PROPOSAL_STATUS_NAMES[number];
/** Decode the authoritative execute/resolve return value, including terminal non-execution. */
export function decodeProposalOutcome(data: Uint8Array): ProposalOutcome {
  if (data.length !== 1 || data[0]! >= PROPOSAL_STATUS_NAMES.length) throw new Error("invalid proposal outcome return data");
  return PROPOSAL_STATUS_NAMES[data[0]!]!;
}

function checkedDigest(digest: Uint8Array | readonly number[]): Uint8Array {
  if (digest.length !== 32 || Array.from(digest).some(value => !Number.isInteger(value) || value < 0 || value > 255)) throw new Error("digest must contain exactly 32 bytes");
  return Uint8Array.from(digest);
}
