import { calculateFee, getTransferFeeConfig, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, unpackMint, type TransferFee } from "@solana/spl-token";
import { SYSVAR_CLOCK_PUBKEY, type Commitment, type Connection } from "@solana/web3.js";
import { address, type AddressLike } from "./address.js";

export interface TransferFeeSchedules {
  olderTransferFee: TransferFee;
  newerTransferFee: TransferFee;
}
export type TransferFeeValuation = "effective" | "admission" | "liquidation";
export interface TransferFeeNotice {
  epoch: bigint;
  effective: TransferFee | null;
  pending: TransferFee | null;
  phase: "none" | "notice" | "finalNotice";
  /** Re-evaluate at this epoch even if no mint account update arrives. */
  nextRecheckEpoch: bigint | null;
}
const U64_MAX = (1n << 64n) - 1n;
function u64(value: bigint): void {
  if (value < 0n || value > U64_MAX) throw new Error("Expected a u64 amount or epoch");
}

/** Schedule selection only; this does not establish position eligibility.
 * Pending schedules are never charged on actual transfers before activation.
 */
export function transferFeeNotice(schedules: TransferFeeSchedules | null, epoch: bigint): TransferFeeNotice {
  u64(epoch);
  if (!schedules) return { epoch, effective: null, pending: null, phase: "none", nextRecheckEpoch: null };
  for (const fee of [schedules.olderTransferFee, schedules.newerTransferFee]) {
    u64(fee.epoch); u64(fee.maximumFee);
    if (!Number.isInteger(fee.transferFeeBasisPoints) || fee.transferFeeBasisPoints < 0 || fee.transferFeeBasisPoints > 10_000) throw new Error("Invalid transfer fee rate");
  }
  const pending = schedules.newerTransferFee.epoch > epoch ? { ...schedules.newerTransferFee } : null;
  const finalNotice = pending !== null && pending.epoch - epoch === 1n;
  return {
    epoch, effective: { ...(pending ? schedules.olderTransferFee : schedules.newerTransferFee) }, pending,
    phase: pending ? (finalNotice ? "finalNotice" : "notice") : "none",
    nextRecheckEpoch: pending ? (finalNotice ? pending.epoch : pending.epoch - 1n) : null,
  };
}

/** Exact post-fee credit with SPL ceiling rounding and each schedule's own cap.
 * Admission considers all announced changes; liquidation considers only a
 * change activating next epoch. Never combine one schedule's rate with another's cap.
 */
export function transferFeeCredit(gross: bigint, notice: TransferFeeNotice, valuation: TransferFeeValuation = "effective"): bigint {
  u64(gross);
  if (!["effective", "admission", "liquidation"].includes(valuation)) throw new Error("Invalid fee valuation");
  let credit = notice.effective ? gross - calculateFee(notice.effective, gross) : gross;
  if (notice.pending && (valuation === "admission" || (valuation === "liquidation" && notice.phase === "finalNotice"))) {
    const pendingCredit = gross - calculateFee(notice.pending, gross);
    if (pendingCredit < credit) credit = pendingCredit;
  }
  return credit;
}

export interface MintTransferFeeNotice extends TransferFeeNotice {
  mint: string;
  tokenProgram: string;
  feeAuthority: string | null;
  /** Changes on epoch rollover, scheduling, cancellation or authority changes. */
  refreshKey: string;
}
export interface TransferFeeNoticeSnapshot {
  slot: number;
  epoch: bigint;
  mints: MintTransferFeeNotice[];
}

/** Read up to 99 mints and Clock from one confirmed RPC bank. This is a
 * monitoring snapshot, not a locked execution quote. Re-read before simulation
 * and signing; obtain liquidation eligibility/payment from the on-chain preview.
 */
export async function fetchTransferFeeNotices(
  connection: Pick<Connection, "getMultipleAccountsInfoAndContext">,
  mints: readonly AddressLike[],
  options: { commitment?: Commitment; minContextSlot?: number } = {},
): Promise<TransferFeeNoticeSnapshot> {
  if (mints.length === 0 || mints.length > 99) throw new Error("Expected 1 to 99 mints");
  const keys = mints.map(address);
  const result = await connection.getMultipleAccountsInfoAndContext([...keys, SYSVAR_CLOCK_PUBKEY], { commitment: "confirmed", ...options });
  const slot = result.context.slot;
  const clock = result.value[keys.length];
  if (!Number.isSafeInteger(slot) || slot < (options.minContextSlot ?? 0) || result.value.length !== keys.length + 1 || !clock || clock.executable || clock.owner.toBase58() !== "Sysvar1111111111111111111111111111111111111" || clock.data.length !== 40 || clock.data.readBigUInt64LE(0) !== BigInt(slot)) throw new Error("Invalid transfer-fee snapshot or Clock");
  const epoch = clock.data.readBigUInt64LE(16);
  return { slot, epoch, mints: keys.map((key, index) => {
    const info = result.value[index];
    if (!info || info.executable || (!info.owner.equals(TOKEN_PROGRAM_ID) && !info.owner.equals(TOKEN_2022_PROGRAM_ID))) throw new Error("Invalid mint owner");
    const mint = unpackMint(key, info, info.owner);
    if (!mint.isInitialized) throw new Error("Uninitialized mint");
    const config = getTransferFeeConfig(mint);
    const notice = transferFeeNotice(config, epoch);
    const authority = config?.transferFeeConfigAuthority?.toBase58();
    const feeAuthority = authority && authority !== "11111111111111111111111111111111" ? authority : null;
    const fees = config ? [config.olderTransferFee, config.newerTransferFee].map(f => `${f.epoch}:${f.transferFeeBasisPoints}:${f.maximumFee}`).join("/") : "none";
    return { ...notice, mint: key.toBase58(), tokenProgram: info.owner.toBase58(), feeAuthority,
      refreshKey: `${key.toBase58()}/${info.owner.toBase58()}/${epoch}/${feeAuthority}/${fees}` };
  }) };
}
