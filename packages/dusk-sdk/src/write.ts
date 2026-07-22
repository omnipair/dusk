import type { Program } from "@coral-xyz/anchor";
import { getMint, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { SystemProgram, Transaction, type AccountMeta, type TransactionInstruction } from "@solana/web3.js";

import { address, normalizeAccountKeys, type AddressLike } from "./address.js";
import {
  deriveFutarchyAuthorityAddress,
  deriveMarketInterestVaultAddress,
  deriveReferralPartnerAddress,
} from "./constants.js";
import {
  assertReferralInterestShareBps,
  referralAccrualAddresses,
  resolveTransferHookAccountMetas,
  tokenProgramForMint,
  type TransferHookTransfer,
} from "./referral.js";
import type { Dusk } from "./types_v2.js";

export type DuskInstructionName = Dusk["instructions"][number]["name"];
export type DuskInstructionArgs = unknown[] | unknown | undefined;
export type DuskAccounts = Record<string, unknown>;

export interface DuskBuildOptions {
  accounts?: DuskAccounts;
  remainingAccounts?: AccountMeta[];
}

export type ReferredActionName = "borrow" | "openLeverage";

export interface ReferredActionOptions extends DuskBuildOptions {
  accounts: DuskAccounts;
  payer: AddressLike;
  referrer: AddressLike;
  market: AddressLike;
  debtMint: AddressLike;
  transferHookTransfers?: readonly TransferHookTransfer[];
}

export interface ReferredActionBuild {
  referralPartner: ReturnType<typeof deriveReferralPartnerAddress>[0];
  referralAccrual: ReturnType<typeof deriveReferralPartnerAddress>[0];
  setupInstruction: TransactionInstruction;
  actionInstruction: TransactionInstruction;
  transaction: Transaction;
}

type AnchorMethodBuilder = {
  accounts(accounts: DuskAccounts): AnchorMethodBuilder;
  remainingAccounts(accounts: AccountMeta[]): AnchorMethodBuilder;
  instruction(): Promise<TransactionInstruction>;
  transaction(): Promise<Transaction>;
  rpc(): Promise<string>;
};

type AnchorMethods = Record<string, (...args: unknown[]) => AnchorMethodBuilder>;

export class DuskWrite {
  constructor(readonly program: Program<Dusk>) {}

  method(name: DuskInstructionName, args?: DuskInstructionArgs): AnchorMethodBuilder {
    const method = (this.program.methods as unknown as AnchorMethods)[name];
    if (!method) {
      throw new Error(`Unknown Dusk instruction: ${name}`);
    }
    return method(...normalizeArgs(args));
  }

  builder(
    name: DuskInstructionName,
    args?: DuskInstructionArgs,
    options: DuskBuildOptions = {}
  ): AnchorMethodBuilder {
    let builder = this.method(name, args);
    if (options.accounts) {
      builder = builder.accounts(normalizeAccountKeys(options.accounts));
    }
    if (options.remainingAccounts?.length) {
      builder = builder.remainingAccounts(options.remainingAccounts);
    }
    return builder;
  }

  instruction(
    name: DuskInstructionName,
    args?: DuskInstructionArgs,
    options?: DuskBuildOptions
  ): Promise<TransactionInstruction> {
    return this.builder(name, args, options).instruction();
  }

  transaction(
    name: DuskInstructionName,
    args?: DuskInstructionArgs,
    options?: DuskBuildOptions
  ): Promise<Transaction> {
    return this.builder(name, args, options).transaction();
  }

  rpc(name: DuskInstructionName, args?: DuskInstructionArgs, options?: DuskBuildOptions) {
    return this.builder(name, args, options).rpc();
  }

  async configureReferralPartnerInstruction(params: {
    authoritySigner: AddressLike;
    referrer: AddressLike;
    interestShareBps: number;
    active: boolean;
    futarchyAuthority?: AddressLike;
  }): Promise<TransactionInstruction> {
    assertReferralInterestShareBps(params.interestShareBps);
    const referrer = address(params.referrer);
    return this.instruction(
      "configureReferralPartner",
      {
        referrer,
        interestShareBps: params.interestShareBps,
        active: params.active,
      },
      {
        accounts: {
          authoritySigner: address(params.authoritySigner),
          futarchyAuthority:
            params.futarchyAuthority ?? deriveFutarchyAuthorityAddress()[0],
          referralPartner: deriveReferralPartnerAddress(referrer)[0],
          systemProgram: SystemProgram.programId,
        },
      }
    );
  }

  async configureReferralPartnerTransaction(
    params: Parameters<DuskWrite["configureReferralPartnerInstruction"]>[0]
  ): Promise<Transaction> {
    return new Transaction().add(await this.configureReferralPartnerInstruction(params));
  }

  async initializeReferralAccrualInstruction(params: {
    payer: AddressLike;
    referrer: AddressLike;
    market: AddressLike;
    assetMint: AddressLike;
  }): Promise<TransactionInstruction> {
    const referral = referralAccrualAddresses(params);
    return this.instruction("initializeReferralAccrual", undefined, {
      accounts: {
        payer: address(params.payer),
        referralPartner: referral.referralPartner,
        market: address(params.market),
        assetMint: address(params.assetMint),
        referralAccrual: referral.referralAccrual,
        systemProgram: SystemProgram.programId,
      },
    });
  }

  async initializeReferralAccrualTransaction(
    params: Parameters<DuskWrite["initializeReferralAccrualInstruction"]>[0]
  ): Promise<Transaction> {
    return new Transaction().add(
      await this.initializeReferralAccrualInstruction(params)
    );
  }

  async referredAction(
    name: ReferredActionName,
    args: Record<string, unknown>,
    options: ReferredActionOptions
  ): Promise<ReferredActionBuild> {
    const referral = referralAccrualAddresses({
      referrer: options.referrer,
      market: options.market,
      assetMint: options.debtMint,
    });
    const setupInstruction = await this.initializeReferralAccrualInstruction({
      payer: options.payer,
      referrer: options.referrer,
      market: options.market,
      assetMint: options.debtMint,
    });
    const hookAccounts = options.transferHookTransfers?.length
      ? await resolveTransferHookAccountMetas(
          this.program.provider.connection,
          options.transferHookTransfers
        )
      : [];
    const actionInstruction = await this.instruction(
      name,
      {
        ...args,
        referrer: address(options.referrer),
      },
      {
        accounts: {
          ...options.accounts,
          referralPartner: referral.referralPartner,
          referralAccrual: referral.referralAccrual,
        },
        remainingAccounts: mergeAccountMetas(options.remainingAccounts ?? [], hookAccounts),
      }
    );
    return {
      referralPartner: referral.referralPartner,
      referralAccrual: referral.referralAccrual,
      setupInstruction,
      actionInstruction,
      transaction: new Transaction().add(setupInstruction, actionInstruction),
    };
  }

  referredBorrow(args: Record<string, unknown>, options: ReferredActionOptions) {
    return this.referredAction("borrow", args, options);
  }

  referredOpenLeverage(args: Record<string, unknown>, options: ReferredActionOptions) {
    return this.referredAction("openLeverage", args, options);
  }

  async setReferralRecipientInstruction(params: {
    authority: AddressLike;
    recipient: AddressLike;
  }): Promise<TransactionInstruction> {
    const authority = address(params.authority);
    return this.instruction(
      "setReferralRecipient",
      { recipient: address(params.recipient) },
      {
        accounts: {
          authority,
          referralPartner: deriveReferralPartnerAddress(authority)[0],
        },
      }
    );
  }

  async setReferralRecipientTransaction(params: {
    authority: AddressLike;
    recipient: AddressLike;
  }): Promise<Transaction> {
    return new Transaction().add(await this.setReferralRecipientInstruction(params));
  }

  async claimReferralInterestInstruction(params: {
    authority: AddressLike;
    market: AddressLike;
    mint: AddressLike;
    interestVault?: AddressLike;
    recipientTokenAccount: AddressLike;
    remainingAccounts?: AccountMeta[];
  }): Promise<TransactionInstruction> {
    const authority = address(params.authority);
    const market = address(params.market);
    const mintKey = address(params.mint);
    const referral = referralAccrualAddresses({
      referrer: authority,
      market,
      assetMint: mintKey,
    });
    const interestVault = address(
      params.interestVault ?? deriveMarketInterestVaultAddress(market, mintKey)[0]
    );
    const recipientTokenAccount = address(params.recipientTokenAccount);
    const tokenProgram = await tokenProgramForMint(
      this.program.provider.connection,
      mintKey
    );
    const [accrual, mint] = await Promise.all([
      this.program.account.referralAccrual.fetch(referral.referralAccrual),
      getMint(
        this.program.provider.connection,
        mintKey,
        undefined,
        tokenProgram
      ),
    ]);
    const hookAccounts = await resolveTransferHookAccountMetas(
      this.program.provider.connection,
      [
        {
          source: interestVault,
          mint: mintKey,
          destination: recipientTokenAccount,
          authority: market,
          amount: accrual.amount,
          decimals: mint.decimals,
          tokenProgram,
        },
      ]
    );
    return this.instruction("claimReferralInterest", undefined, {
      accounts: {
        market,
        authority,
        referralPartner: referral.referralPartner,
        assetMint: mintKey,
        referralAccrual: referral.referralAccrual,
        interestVault,
        recipientTokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
      },
      remainingAccounts: mergeAccountMetas(params.remainingAccounts ?? [], hookAccounts),
    });
  }

  async claimReferralInterestTransaction(
    params: Parameters<DuskWrite["claimReferralInterestInstruction"]>[0]
  ): Promise<Transaction> {
    return new Transaction().add(
      await this.claimReferralInterestInstruction(params)
    );
  }
}

function normalizeArgs(args: DuskInstructionArgs): unknown[] {
  if (args === undefined) {
    return [];
  }
  return Array.isArray(args) ? args : [args];
}

function mergeAccountMetas(...groups: readonly AccountMeta[][]): AccountMeta[] {
  const merged = new Map<string, AccountMeta>();
  for (const group of groups) {
    for (const meta of group) {
      const key = meta.pubkey.toBase58();
      const current = merged.get(key);
      merged.set(key, {
        pubkey: meta.pubkey,
        isSigner: Boolean(current?.isSigner || meta.isSigner),
        isWritable: Boolean(current?.isWritable || meta.isWritable),
      });
    }
  }
  return [...merged.values()];
}
