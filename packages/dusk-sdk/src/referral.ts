import {
  createTransferCheckedWithTransferHookInstruction,
  getMint,
  getTransferHook,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import {
  PublicKey,
  type AccountMeta,
  type Commitment,
  type Connection,
} from "@solana/web3.js";

import { address, type AddressLike } from "./address.js";
import {
  deriveReferralAccrualAddress,
  deriveReferralProfileAddress,
} from "./constants.js";

export const REFERRAL_BPS_DENOMINATOR = 10_000;
export const MAX_REFERRAL_INTEREST_SHARE_BPS = REFERRAL_BPS_DENOMINATOR;

export type IntegerLike = bigint | number | string | { toString(): string };

export interface ReferralInterestQuote {
  interestPaid: bigint;
  interestVaultCredit: bigint;
  protocolInterestRevenue: bigint;
  interestShareBps: number;
  referralAmount: bigint;
}

export function referralBindingInterestShareBps(params: {
  configuredShareBps: number;
  maxShareBps: number;
  active?: boolean;
}): number {
  assertReferralInterestShareBps(params.configuredShareBps);
  assertReferralInterestShareBps(params.maxShareBps);
  if (params.active === false) throw new Error("Referral profile is inactive");
  return Math.min(params.configuredShareBps, params.maxShareBps);
}

export function quoteReferralInterestShare(params: {
  interestPaid: IntegerLike;
  interestVaultCredit?: IntegerLike;
  protocolInterestBps: number;
  interestShareBps: number;
}): ReferralInterestQuote {
  assertBps(params.protocolInterestBps, "protocol interest share");
  assertReferralInterestShareBps(params.interestShareBps);
  const interestPaid = BigInt(params.interestPaid.toString());
  if (interestPaid < 0n) throw new Error("interestPaid must be non-negative");
  const interestVaultCredit = BigInt(
    (params.interestVaultCredit ?? params.interestPaid).toString()
  );
  if (interestVaultCredit < 0n || interestVaultCredit > interestPaid) {
    throw new Error("interestVaultCredit must be between 0 and interestPaid");
  }

  const protocolInterestRevenue =
    (interestVaultCredit * BigInt(params.protocolInterestBps)) /
    BigInt(REFERRAL_BPS_DENOMINATOR);
  const referralAmount =
    (protocolInterestRevenue * BigInt(params.interestShareBps)) /
    BigInt(REFERRAL_BPS_DENOMINATOR);
  return {
    interestPaid,
    interestVaultCredit,
    protocolInterestRevenue,
    interestShareBps: params.interestShareBps,
    referralAmount,
  };
}

export function assertReferralInterestShareBps(shareBps: number): void {
  assertBps(shareBps, "referral interest share");
}

export async function tokenProgramForMint(
  connection: Connection,
  mint: AddressLike,
  commitment?: Commitment
): Promise<PublicKey> {
  const mintKey = address(mint);
  const account = await connection.getAccountInfo(mintKey, commitment);
  if (!account) {
    throw new Error(`Mint account not found: ${mintKey.toBase58()}`);
  }
  if (account.owner.equals(TOKEN_PROGRAM_ID) || account.owner.equals(TOKEN_2022_PROGRAM_ID)) {
    return account.owner;
  }
  throw new Error(`Unsupported mint owner: ${account.owner.toBase58()}`);
}

export interface ReferralAccrualAddresses {
  referralProfile: PublicKey;
  referralAccrual: PublicKey;
}

export function referralAccrualAddresses(params: {
  referrer: AddressLike;
  market: AddressLike;
  assetMint: AddressLike;
}): ReferralAccrualAddresses {
  const referrer = address(params.referrer);
  const market = address(params.market);
  const assetMint = address(params.assetMint);
  const [referralProfile] = deriveReferralProfileAddress(referrer);
  const [referralAccrual] = deriveReferralAccrualAddress(
    referralProfile,
    market,
    assetMint
  );
  return { referralProfile, referralAccrual };
}

function assertBps(value: number, label: string): void {
  if (!Number.isInteger(value) || value < 0 || value > REFERRAL_BPS_DENOMINATOR) {
    throw new Error(`${label} must be between 0 and ${REFERRAL_BPS_DENOMINATOR} bps`);
  }
}

export interface TransferHookTransfer {
  source: AddressLike;
  mint: AddressLike;
  destination: AddressLike;
  authority: AddressLike;
  amount: IntegerLike;
  decimals: number;
  tokenProgram?: AddressLike;
}

/**
 * Resolve the union of Token-2022 hook accounts required by one or more CPIs.
 * Dusk resolves the correct subset on-chain for each exact transfer.
 */
export async function resolveTransferHookAccountMetas(
  connection: Connection,
  transfers: readonly TransferHookTransfer[],
  commitment?: Commitment
): Promise<AccountMeta[]> {
  const merged = new Map<string, AccountMeta>();
  for (const transfer of transfers) {
    const mint = address(transfer.mint);
    const tokenProgram = transfer.tokenProgram
      ? address(transfer.tokenProgram)
      : await tokenProgramForMint(connection, mint, commitment);
    if (tokenProgram.equals(TOKEN_PROGRAM_ID)) continue;
    const instruction = await createTransferCheckedWithTransferHookInstruction(
      connection,
      address(transfer.source),
      mint,
      address(transfer.destination),
      address(transfer.authority),
      BigInt(transfer.amount.toString()),
      transfer.decimals,
      [],
      commitment,
      tokenProgram
    );
    const transferHook = getTransferHook(
      await getMint(connection, mint, commitment, tokenProgram)
    );
    for (const meta of instruction.keys.slice(4)) {
      const key = meta.pubkey.toBase58();
      const current = merged.get(key);
      merged.set(key, {
        pubkey: meta.pubkey,
        isSigner: Boolean(current?.isSigner || meta.isSigner),
        isWritable: Boolean(current?.isWritable || meta.isWritable),
      });
    }
    if (transferHook) {
      const key = transferHook.programId.toBase58();
      const current = merged.get(key);
      merged.set(key, {
        pubkey: transferHook.programId,
        isSigner: Boolean(current?.isSigner),
        isWritable: Boolean(current?.isWritable),
      });
    }
  }
  return [...merged.values()];
}
