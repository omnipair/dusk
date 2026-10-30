import { ComputeBudgetProgram, PublicKey, type AccountMeta } from "@solana/web3.js";
import type { BN } from "@coral-xyz/anchor";
import { address, type AddressLike } from "./address.js";
import { governanceIntegerBN, type GovernanceIntegerLike } from "./governance.js";

export const NORMALIZED_ORACLE_V1 = 1;
export const PYTH_RATIO_V2 = 2;
export const PYTH_RECEIVER_PROGRAM = new PublicKey("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");

export interface OraclePolicyInput {
  format: number;
  provider: AddressLike;
  baseAccount: AddressLike;
  quoteAccount: AddressLike;
  baseFeedId: readonly number[] | Uint8Array;
  quoteFeedId: readonly number[] | Uint8Array;
  lambdaBps: number;
  adjustmentCapNad: GovernanceIntegerLike;
  maxAgeSeconds: number;
  maxAgeSlots: GovernanceIntegerLike;
  maxConfidenceBps: number;
  maxPublishSkewSeconds: number;
}
export interface OraclePolicy extends Omit<OraclePolicyInput,
  "provider" | "baseAccount" | "quoteAccount" | "baseFeedId" | "quoteFeedId" | "adjustmentCapNad" | "maxAgeSlots"> {
  provider: PublicKey;
  baseAccount: PublicKey;
  quoteAccount: PublicKey;
  baseFeedId: number[];
  quoteFeedId: number[];
  adjustmentCapNad: BN;
  maxAgeSlots: BN;
}

/** Validate the policy's mathematical bounds and source identity for any market. */
export function oraclePolicy(input: OraclePolicyInput): OraclePolicy {
  const integer = (n: number, max: number, label: string) => {
    if (!Number.isInteger(n) || n < 0 || n > max) throw new Error(`invalid ${label}`);
    return n;
  };
  const id = (v: readonly number[] | Uint8Array) => {
    if (v.length !== 32 || Array.from(v).some(n => !Number.isInteger(n) || n < 0 || n > 255))
      throw new Error("oracle feed id must contain 32 bytes");
    return Array.from(v);
  };
  const policy: OraclePolicy = {
    format: integer(input.format, 255, "oracle format"), provider: address(input.provider),
    baseAccount: address(input.baseAccount), quoteAccount: address(input.quoteAccount),
    baseFeedId: id(input.baseFeedId), quoteFeedId: id(input.quoteFeedId),
    lambdaBps: integer(input.lambdaBps, 9999, "lambdaBps"),
    adjustmentCapNad: governanceIntegerBN(input.adjustmentCapNad, "adjustmentCapNad"),
    maxAgeSeconds: integer(input.maxAgeSeconds, 0xffff_ffff, "maxAgeSeconds"),
    maxAgeSlots: governanceIntegerBN(input.maxAgeSlots, "maxAgeSlots"),
    maxConfidenceBps: integer(input.maxConfidenceBps, 9999, "maxConfidenceBps"),
    maxPublishSkewSeconds: integer(input.maxPublishSkewSeconds, 0xffff_ffff, "maxPublishSkewSeconds"),
  };
  const zero = PublicKey.default;
  const emptyId = (v: number[]) => v.every(n => n === 0);
  if (policy.format === 0 && policy.lambdaBps === 0 && policy.provider.equals(zero)
    && policy.baseAccount.equals(zero) && policy.quoteAccount.equals(zero)
    && emptyId(policy.baseFeedId) && emptyId(policy.quoteFeedId) && policy.adjustmentCapNad.isZero()
    && policy.maxAgeSeconds === 0 && policy.maxAgeSlots.isZero() && policy.maxConfidenceBps === 0
    && policy.maxPublishSkewSeconds === 0) return policy;
  if (BigInt(policy.adjustmentCapNad.toString()) <= 1_000_000_000n || policy.maxAgeSeconds === 0
    || policy.maxAgeSlots.isZero() || policy.provider.equals(zero) || policy.baseAccount.equals(zero)
    || emptyId(policy.baseFeedId)) throw new Error("invalid oracle policy");
  if (policy.format === NORMALIZED_ORACLE_V1) {
    if (!policy.quoteAccount.equals(zero) || !emptyId(policy.quoteFeedId) || policy.maxPublishSkewSeconds !== 0)
      throw new Error("normalized report must identify one pair account");
  } else if (policy.format === PYTH_RATIO_V2) {
    if (!policy.provider.equals(PYTH_RECEIVER_PROGRAM) || policy.quoteAccount.equals(zero)
      || policy.baseAccount.equals(policy.quoteAccount) || emptyId(policy.quoteFeedId)
      || policy.baseFeedId.every((v, i) => v === policy.quoteFeedId[i]))
      throw new Error("Pyth ratio requires two distinct fully verified feeds");
  } else throw new Error("unsupported oracle format");
  return policy;
}

/** Zero weight has no feed-account dependency, even with a retained source policy. */
export function oracleReadAccounts(policy?: OraclePolicyInput): AccountMeta[] {
  if (!policy || policy.lambdaBps === 0) return [];
  const checked = oraclePolicy(policy);
  return [checked.baseAccount, ...(checked.format === PYTH_RATIO_V2 ? [checked.quoteAccount] : [])]
    .map(pubkey => ({ pubkey, isSigner: false, isWritable: false }));
}

/** Add once before composing positive-weight instructions. Raw instruction
 * builders cannot attach a transaction budget. Index-bound flash plans must
 * include these before their indices are bound; simulate the complete route.
 */
export function oracleComputeBudgetInstructions(policy?: Pick<OraclePolicyInput, "lambdaBps">) {
  return policy && policy.lambdaBps > 0 ? [
    ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
    ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
  ] : [];
}

/** Exact Borsh OraclePolicy payload, also used in proposal digests. */
export function encodeOraclePolicy(input: OraclePolicyInput): Uint8Array {
  const p = oraclePolicy(input);
  const data = Buffer.alloc(189);
  let offset = 0;
  const bytes = (v: Uint8Array | number[]) => { data.set(v, offset); offset += v.length; };
  const u16 = (v: number) => { data.writeUInt16LE(v, offset); offset += 2; };
  const u32 = (v: number) => { data.writeUInt32LE(v, offset); offset += 4; };
  const u64 = (v: BN) => { data.writeBigUInt64LE(BigInt(v.toString()), offset); offset += 8; };
  data[offset++] = p.format;
  bytes(p.provider.toBytes()); bytes(p.baseAccount.toBytes()); bytes(p.quoteAccount.toBytes());
  bytes(p.baseFeedId); bytes(p.quoteFeedId); u16(p.lambdaBps); u64(p.adjustmentCapNad);
  u32(p.maxAgeSeconds); u64(p.maxAgeSlots); u16(p.maxConfidenceBps); u32(p.maxPublishSkewSeconds);
  return data;
}
