import {
  createAssociatedTokenAccountIdempotentInstruction,
  createTransferCheckedWithTransferHookInstruction,
  getAssociatedTokenAddressSync,
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
  type TransactionInstruction,
} from "@solana/web3.js";

import { address, type AddressLike } from "./address.js";
import { deriveReferralProfileAddress } from "./constants.js";

export const DEFAULT_REFERRAL_ORIGINATION_FEE_BPS = 10;
export const MAX_REFERRAL_ORIGINATION_FEE_BPS = 25;

export type IntegerLike = bigint | number | string | { toString(): string };

export interface ReferralFeeQuote {
  requestedPrincipal: bigint;
  configuredFeeBps: number;
  feeDebit: bigint;
  grossDebt: bigint;
}

export function quoteReferralOriginationFee(
  requestedPrincipal: IntegerLike,
  configuredFeeBps: number
): ReferralFeeQuote {
  assertReferralFeeBps(configuredFeeBps);
  const requested = BigInt(requestedPrincipal.toString());
  if (requested < 0n) {
    throw new Error("requestedPrincipal must be non-negative");
  }
  const feeDebit = (requested * BigInt(configuredFeeBps) + 9_999n) / 10_000n;
  return {
    requestedPrincipal: requested,
    configuredFeeBps,
    feeDebit,
    grossDebt: requested + feeDebit,
  };
}

export function assertReferralFeeBps(configuredFeeBps: number): void {
  if (
    !Number.isInteger(configuredFeeBps) ||
    configuredFeeBps < 0 ||
    configuredFeeBps > MAX_REFERRAL_ORIGINATION_FEE_BPS
  ) {
    throw new Error(
      `referral origination fee must be between 0 and ${MAX_REFERRAL_ORIGINATION_FEE_BPS} bps`
    );
  }
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

export interface ReferralVaultAddresses {
  referralProfile: PublicKey;
  referralVault: PublicKey;
  tokenProgram: PublicKey;
}

export async function referralVaultAddresses(params: {
  connection: Connection;
  referrer: AddressLike;
  mint: AddressLike;
  commitment?: Commitment;
}): Promise<ReferralVaultAddresses> {
  const referrer = address(params.referrer);
  const mint = address(params.mint);
  const [referralProfile] = deriveReferralProfileAddress(referrer);
  const tokenProgram = await tokenProgramForMint(params.connection, mint, params.commitment);
  const referralVault = getAssociatedTokenAddressSync(
    mint,
    referralProfile,
    true,
    tokenProgram
  );
  return { referralProfile, referralVault, tokenProgram };
}

export async function buildReferralVaultSetupInstruction(params: {
  connection: Connection;
  payer: AddressLike;
  referrer: AddressLike;
  mint: AddressLike;
  commitment?: Commitment;
}): Promise<ReferralVaultAddresses & { instruction: TransactionInstruction }> {
  const mint = address(params.mint);
  const addresses = await referralVaultAddresses(params);
  return {
    ...addresses,
    instruction: createAssociatedTokenAccountIdempotentInstruction(
      address(params.payer),
      addresses.referralVault,
      addresses.referralProfile,
      mint,
      addresses.tokenProgram
    ),
  };
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
