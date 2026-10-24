import type { Program } from "@coral-xyz/anchor";
import { getAssociatedTokenAddressSync, TOKEN_2022_PROGRAM_ID } from "@solana/spl-token";
import { PublicKey, SystemProgram, Transaction, type AccountMeta } from "@solana/web3.js";
import { address, type AddressLike } from "./address.js";
import { deriveGovernanceBalanceAddress, deriveGovernanceSupportAddress, deriveParameterProposalAddress, deriveYieldAccountAddress } from "./constants.js";
import { anchorParameterUpdate, assertProposalMetadata, canonicalParameterUpdates, governanceIntegerBN, type GovernanceIntegerLike, type ParameterUpdate, type ProposalMetadataV1 } from "./governance.js";
import type { Dusk } from "./types_v2.js";
import type { GovernanceBalance, GovernanceSupport, Market, ParameterProposal } from "./type-aliases.js";
import { DuskWrite, type DuskInstructionName, type GovernanceHolderAccountOverrides } from "./write.js";

export interface GovernanceOwnerParams extends GovernanceHolderAccountOverrides {
  market: AddressLike;
  owner: AddressLike;
}
export interface GovernanceContribution {
  owner: AddressLike;
  /** Raw yLP atoms, taken only from this owner's uncommitted balance. */
  amount: GovernanceIntegerLike;
}
export interface CreateGovernanceProposalParams {
  market: AddressLike;
  proposer: AddressLike;
  nonce: GovernanceIntegerLike;
  updates: readonly ParameterUpdate[];
  metadata: ProposalMetadataV1;
  contributions: readonly GovernanceContribution[];
}
export interface SupportGovernanceProposalParams {
  market: AddressLike;
  proposal: AddressLike;
  authority: AddressLike;
  digest: Uint8Array | readonly number[];
  contributions: readonly GovernanceContribution[];
}
export interface WithdrawGovernanceSupportParams {
  market: AddressLike;
  proposal: AddressLike;
  owner: AddressLike;
  /** The owner or their current delegate. */
  authority: AddressLike;
  amount?: GovernanceIntegerLike;
  /** Defaults are fetched from the proposal/support accounts. */
  proposer?: AddressLike;
  rentRefund?: AddressLike;
}

/** Reusable yLP voting balances. These builders never sign or submit transactions. */
export class DuskGovernance {
  private readonly writer: DuskWrite;
  constructor(readonly program: Program<Dusk>) { this.writer = new DuskWrite(program); }

  // Keep account decoding scoped to the requested account. Expanding the
  // entire generated IDL here exceeds TypeScript's recursive type depth.
  private accountClient<T>(name: string) {
    return (this.program.account as unknown as Record<string, {
      fetch(address: PublicKey): Promise<T>;
      fetchNullable(address: PublicKey): Promise<T | null>;
      all(filters?: unknown[]): Promise<Array<{ publicKey: PublicKey; account: T }>>;
    }>)[name];
  }

  async getBalance(market: AddressLike, owner: AddressLike) {
    const [publicKey] = deriveGovernanceBalanceAddress(address(market), address(owner), this.program.programId);
    const account = await this.accountClient<GovernanceBalance>("governanceBalance").fetchNullable(publicKey);
    return account ? { publicKey, ...account, availableAmount: account.totalAmount.sub(account.committedAmount) } : null;
  }

  async getSupport(proposal: AddressLike, market: AddressLike, owner: AddressLike) {
    const [balance] = deriveGovernanceBalanceAddress(address(market), address(owner), this.program.programId);
    const [publicKey] = deriveGovernanceSupportAddress(address(proposal), balance, this.program.programId);
    const account = await this.accountClient<GovernanceSupport>("governanceSupport").fetchNullable(publicKey);
    return account ? { publicKey, ...account } : null;
  }

  /** All allocations, including ended proposals awaiting release, for one owner. */
  async listSupport(market: AddressLike, owner: AddressLike) {
    const [balance] = deriveGovernanceBalanceAddress(address(market), address(owner), this.program.programId);
    return this.accountClient<GovernanceSupport>("governanceSupport").all([{ memcmp: { offset: 40, bytes: balance.toBase58() } }]);
  }

  deposit(params: GovernanceOwnerParams & { amount: GovernanceIntegerLike }) {
    return this.manage(params, { deposit: { amount: positiveAmount(params.amount) } });
  }
  withdraw(params: GovernanceOwnerParams & { amount: GovernanceIntegerLike }) {
    return this.manage(params, { withdraw: { amount: positiveAmount(params.amount) } });
  }
  /** Only the owner signs. null revokes immediately without changing any allocations. */
  async setDelegate(params: { market: AddressLike; owner: AddressLike; delegate: AddressLike | null }) {
    const owner = address(params.owner);
    const [governanceBalance] = deriveGovernanceBalanceAddress(address(params.market), owner, this.program.programId);
    const instruction = await this.writer.instruction("setGovernanceDelegate" as DuskInstructionName,
      params.delegate === null ? PublicKey.default : address(params.delegate), { accounts: { owner, governanceBalance } });
    return { governanceBalance, instruction, transaction: new Transaction().add(instruction) };
  }
  /** Credits virtual yield to the owner's YieldAccounts; use the existing harvest helper to collect it. */
  claimYield(params: GovernanceOwnerParams) {
    return this.manage(params, { claimYield: {} });
  }

  private async manage(params: GovernanceOwnerParams, action: unknown) {
    const market = address(params.market), owner = address(params.owner);
    const state = await this.accountClient<Market>("market").fetch(market);
    const ylpMint = address(params.ylpMint ?? state.ylpMint);
    const [governanceBalance] = deriveGovernanceBalanceAddress(market, owner, this.program.programId);
    const instruction = await this.writer.instruction("manageGovernanceBalance" as DuskInstructionName, action, { accounts: {
      owner, market, governanceBalance, ylpMint,
      ownerYlpAccount: address(params.holderYlpAccount ?? getAssociatedTokenAddressSync(ylpMint, owner, true, TOKEN_2022_PROGRAM_ID)),
      baseYieldAccount: address(params.baseYieldAccount ?? deriveYieldAccountAddress(market, owner, ylpMint, state.baseSide.assetMint, "ylp", this.program.programId)[0]),
      quoteYieldAccount: address(params.quoteYieldAccount ?? deriveYieldAccountAddress(market, owner, ylpMint, state.quoteSide.assetMint, "ylp", this.program.programId)[0]),
      token2022Program: TOKEN_2022_PROGRAM_ID, systemProgram: SystemProgram.programId,
    }});
    return { governanceBalance, instruction, transaction: new Transaction().add(instruction) };
  }

  async createProposal(params: CreateGovernanceProposalParams) {
    assertProposalMetadata(params.metadata);
    const market = address(params.market), proposer = address(params.proposer);
    const [proposal] = deriveParameterProposalAddress(market, proposer, params.nonce, this.program.programId);
    const state = await this.accountClient<Market>("market").fetch(market);
    const batch = this.contributions(market, proposal, params.contributions);
    const instruction = await this.writer.instruction("createGovernanceProposal" as DuskInstructionName, {
      nonce: governanceIntegerBN(params.nonce, "nonce"),
      updates: canonicalParameterUpdates(params.updates).map(anchorParameterUpdate),
      metadata: { ...params.metadata, descriptionSha256: Array.from(params.metadata.descriptionSha256) },
      contributions: batch.amounts,
    }, { accounts: {
      proposer, market, proposal, ylpMint: state.ylpMint, baseHlpYlpVault: state.baseHlpVault.ylpVault,
      quoteHlpYlpVault: state.quoteHlpVault.ylpVault, token2022Program: TOKEN_2022_PROGRAM_ID, systemProgram: SystemProgram.programId,
    }, remainingAccounts: batch.remainingAccounts });
    return { proposal, supports: batch.supports, instruction, transaction: new Transaction().add(instruction) };
  }

  async supportProposal(params: SupportGovernanceProposalParams) {
    const market = address(params.market), proposal = address(params.proposal);
    const digest = Array.from(params.digest);
    if (digest.length !== 32 || digest.some((v) => !Number.isInteger(v) || v < 0 || v > 255)) throw new Error("Proposal digest must contain 32 bytes");
    const state = await this.accountClient<Market>("market").fetch(market);
    const batch = this.contributions(market, proposal, params.contributions);
    const instruction = await this.writer.instruction("supportGovernanceProposal" as DuskInstructionName, {
      digest, contributions: batch.amounts,
    }, { accounts: {
      authority: address(params.authority), market, proposal, ylpMint: state.ylpMint,
      baseHlpYlpVault: state.baseHlpVault.ylpVault, quoteHlpYlpVault: state.quoteHlpVault.ylpVault, systemProgram: SystemProgram.programId,
    }, remainingAccounts: batch.remainingAccounts });
    return { proposal, supports: batch.supports, instruction, transaction: new Transaction().add(instruction) };
  }

  async withdrawSupport(params: WithdrawGovernanceSupportParams) {
    const market = address(params.market), proposal = address(params.proposal);
    const [governanceBalance] = deriveGovernanceBalanceAddress(market, address(params.owner), this.program.programId);
    const [governanceSupport] = deriveGovernanceSupportAddress(proposal, governanceBalance, this.program.programId);
    const proposer = params.proposer ?? (await this.accountClient<ParameterProposal>("parameterProposal").fetch(proposal)).proposer;
    const rentRefund = params.rentRefund ?? (await this.accountClient<GovernanceSupport>("governanceSupport").fetch(governanceSupport)).rentRefund;
    const instruction = await this.writer.instruction("withdrawGovernanceSupport" as DuskInstructionName,
      { amount: params.amount === undefined ? null : positiveAmount(params.amount) }, { accounts: {
        authority: address(params.authority), market, proposal, proposer: address(proposer), governanceBalance, governanceSupport, rentRefund: address(rentRefund),
      }});
    return { proposal, governanceBalance, governanceSupport, instruction, transaction: new Transaction().add(instruction) };
  }

  private contributions(market: PublicKey, proposal: PublicKey, contributions: readonly GovernanceContribution[]) {
    if (!contributions.length) throw new Error("At least one governance contribution is required");
    const supports = contributions.map((contribution) => {
      const owner = address(contribution.owner);
      const [governanceBalance] = deriveGovernanceBalanceAddress(market, owner, this.program.programId);
      const [governanceSupport] = deriveGovernanceSupportAddress(proposal, governanceBalance, this.program.programId);
      return { owner, governanceBalance, governanceSupport, amount: positiveAmount(contribution.amount) };
    }).sort((a, b) => Buffer.compare(a.governanceBalance.toBuffer(), b.governanceBalance.toBuffer()));
    for (let i = 1; i < supports.length; i++) {
      if (supports[i].governanceBalance.equals(supports[i - 1].governanceBalance)) throw new Error("Duplicate governance owner");
    }
    const remainingAccounts: AccountMeta[] = supports.flatMap((s) => [s.governanceBalance, s.governanceSupport].map((pubkey) => ({ pubkey, isSigner: false, isWritable: true })));
    return { supports, amounts: supports.map((s) => s.amount), remainingAccounts };
  }
}

function positiveAmount(value: GovernanceIntegerLike) {
  const amount = governanceIntegerBN(value, "amount");
  if (amount.isZero()) throw new Error("Governance amount must be positive");
  return amount;
}
