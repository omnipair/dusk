import type { Program } from "@coral-xyz/anchor";
import { getAssociatedTokenAddressSync, TOKEN_2022_PROGRAM_ID } from "@solana/spl-token";
import { PublicKey, SystemProgram, Transaction, type AccountMeta } from "@solana/web3.js";
import { address, type AddressLike } from "./address.js";
import { deriveGovernanceBalanceAddress, deriveGovernanceControlAddress, deriveGovernanceSupportAddress, deriveJuniorCohortAddress, deriveParameterProposalAddress, deriveYieldAccountAddress } from "./constants.js";
import { anchorParameterUpdate, assertProposalMetadata, canonicalParameterUpdates, canonicalGovernanceUpdates, anchorGovernanceAction, anchorRevisionConditions, governanceIntegerBN, type GovernanceActionInput, type RevisionCondition, type GovernanceIntegerLike, type ParameterUpdate, type ProposalMetadataV1 } from "./governance.js";
import type { Dusk } from "./types_v2.js";
import type { GovernanceBalance, GovernanceSupport, GovernanceControl, RecoveryBallot, RecoveryVote, Market, ParameterProposal } from "./type-aliases.js";
import { DuskWrite, type DuskInstructionName, type GovernanceHolderAccountOverrides } from "./write.js";

import { anchorRecoveryTarget, deriveRecoveryBallotAddress, deriveRecoveryVoteAddress, MANAGER_POLICY_VERSION, recoveryTargetFromAnchor, type RecoveryTargetInput } from "./management.js";

export interface ManagerProposalParams {
  market: AddressLike; manager: AddressLike; payer: AddressLike;
  nonce: GovernanceIntegerLike; expectedEpoch?: GovernanceIntegerLike;
  updates: readonly ParameterUpdate[]; metadata: ProposalMetadataV1;
}
export interface RecoveryVoteParams {
  market: AddressLike; ballot: AddressLike; source: AddressLike;
  sourceProposal?: AddressLike; authority: AddressLike; payer: AddressLike;
  amount: GovernanceIntegerLike; round?: GovernanceIntegerLike;
}

export interface GovernanceOwnerParams extends GovernanceHolderAccountOverrides {
  market: AddressLike;
  owner: AddressLike;
  /** Defaults to the senior yLP governance balance. */
  kind?: GovernanceLpKind;
  lpMint?: AddressLike;
  ownerLpAccount?: AddressLike;
}
export type GovernanceLpKind = "senior" | "junior";
export interface GovernanceContribution {
  owner: AddressLike;
  /** Raw receipt atoms, taken only from this owner's matching uncommitted balance. */
  amount: GovernanceIntegerLike;
  /** Defaults to senior yLP. */
  kind?: GovernanceLpKind;
}
export interface CreateGovernanceProposalParams {
  market: AddressLike;
  proposer: AddressLike;
  nonce: GovernanceIntegerLike;
  updates: readonly ParameterUpdate[];
  action?: GovernanceActionInput;
  conditions?: readonly RevisionCondition[];
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
  /** The governance balance that supplied this support. Defaults to senior. */
  kind?: GovernanceLpKind;
  /** Original receipt mint when releasing support from a retired junior cohort. */
  lpMint?: AddressLike;
}

/** Reusable senior yLP and current-generation junior LP voting balances. */
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

  async getControl(market: AddressLike) {
    const key = deriveGovernanceControlAddress(address(market), this.program.programId)[0];
    return this.accountClient<GovernanceControl>("governanceControl").fetchNullable(key);
  }

  /** The current manager, or null after LP removal leaves governance-only management. */
  async getManager(market: AddressLike): Promise<PublicKey | null> {
    const control = await this.getControl(market);
    return !control || control.manager.equals(PublicKey.default) ? null : control.manager;
  }

  async electManager(params: Omit<CreateGovernanceProposalParams, "updates" | "action"> & {manager: AddressLike; expectedEpoch?: GovernanceIntegerLike}) {
    const expectedEpoch = params.expectedEpoch ?? (await this.getControl(params.market))?.managerEpoch ?? 0;
    return this.createProposal({...params, updates: [], action: {kind: "appointManager", manager: params.manager, expectedEpoch}});
  }

  async proposeAsManager(params: ManagerProposalParams) {
    assertProposalMetadata(params.metadata);
    const market = address(params.market), manager = address(params.manager), payer = address(params.payer);
    const control = await this.getControl(market);
    const state = await this.accountClient<Market>("market").fetch(market);
    if (!control) throw new Error("this market has no governance control account or elected manager");
    const [proposal] = deriveParameterProposalAddress(market, manager, params.nonce, this.program.programId);
    const updates = canonicalParameterUpdates(params.updates);
    if (updates.some(update => update.kind === "oracle")) throw new Error("oracle policy and feeds require LP governance");
    const instruction = await this.writer.instruction("createManagerProposal" as DuskInstructionName, {
      nonce: governanceIntegerBN(params.nonce, "nonce"),
      expectedEpoch: governanceIntegerBN(params.expectedEpoch ?? control.managerEpoch, "expectedEpoch"),
      policyVersion: governanceIntegerBN(MANAGER_POLICY_VERSION, "policyVersion"),
      updates: updates.map(anchorParameterUpdate),
      metadata: {...params.metadata, descriptionSha256: Array.from(params.metadata.descriptionSha256)},
    }, {accounts: {payer, manager, market, proposal, ylpMint: state.ylpMint, baseHlpYlpVault: state.baseHlpVault.ylpVault, quoteHlpYlpVault: state.quoteHlpVault.ylpVault, systemProgram: SystemProgram.programId}});
    return {proposal, instruction, transaction: new Transaction().add(instruction)};
  }

  async resolveManagerProposal(params: {market: AddressLike; proposal: AddressLike; cancel?: boolean; manager?: AddressLike; rentRefund?: AddressLike}) {
    const market = address(params.market), proposal = address(params.proposal);
    const stored = await this.accountClient<ParameterProposal>("parameterProposal").fetch(proposal);
    if (params.cancel && !params.manager) throw new Error("self-cancellation requires the manager signer");
    const instruction = await this.writer.instruction("resolveManagerProposal" as DuskInstructionName, [params.cancel ?? false], {accounts: {
      market, proposal, manager: params.manager ? address(params.manager) : null,
      rentRefund: address(params.rentRefund ?? stored.rentRefund),
    }});
    return {instruction, transaction: new Transaction().add(instruction)};
  }

  async openRecoveryBallot(params: {market: AddressLike; payer: AddressLike; target: RecoveryTargetInput}) {
    const market = address(params.market), payer = address(params.payer);
    const [ballot] = await deriveRecoveryBallotAddress(market, params.target, this.program.programId);
    const instruction = await this.writer.instruction("openRecoveryBallot" as DuskInstructionName, [anchorRecoveryTarget(params.target)], {accounts: {
      payer, market, ballot, systemProgram: SystemProgram.programId,
      targetProposal: params.target.kind === "cancelProposal" ? address(params.target.proposal) : null,
    }});
    return {ballot, instruction, transaction: new Transaction().add(instruction)};
  }

  async openCancellationBallot(params: {market: AddressLike; payer: AddressLike; proposal: AddressLike}) {
    const proposal = await this.accountClient<ParameterProposal>("parameterProposal").fetch(address(params.proposal));
    return this.openRecoveryBallot({...params, target: {kind: "cancelProposal", proposal: params.proposal, digest: proposal.digest}});
  }

  async openRemovalBallot(params: {market: AddressLike; payer: AddressLike; epoch?: GovernanceIntegerLike}) {
    const control = await this.getControl(params.market);
    if (!control || control.manager.equals(PublicKey.default)) throw new Error("market has no manager");
    return this.openRecoveryBallot({...params, target: {kind: "removeManager", epoch: params.epoch ?? control.managerEpoch}});
  }

  async getRecoveryBallot(ballot: AddressLike) {
    return this.accountClient<RecoveryBallot>("recoveryBallot").fetch(address(ballot));
  }

  async voteRecovery(params: RecoveryVoteParams) {
    const market = address(params.market), ballot = address(params.ballot), source = address(params.source);
    const stored = await this.getRecoveryBallot(ballot);
    const state = await this.accountClient<Market>("market").fetch(market);
    const target = recoveryTargetFromAnchor(stored.target);
    const round = params.round ?? stored.round;
    const [vote] = deriveRecoveryVoteAddress(ballot, round, source, this.program.programId);
    const instruction = await this.writer.instruction("voteRecoveryBallot" as DuskInstructionName, {round: governanceIntegerBN(round, "round"), amount: positiveAmount(params.amount)}, {accounts: {
      payer: address(params.payer), authority: address(params.authority), market, ballot, source, vote,
      sourceProposal: params.sourceProposal ? address(params.sourceProposal) : null,
      targetProposal: target.kind === "cancelProposal" ? address(target.proposal) : null,
      ylpMint: state.ylpMint, baseHlpYlpVault: state.baseHlpVault.ylpVault, quoteHlpYlpVault: state.quoteHlpVault.ylpVault,
      systemProgram: SystemProgram.programId,
    }});
    return {vote, instruction, transaction: new Transaction().add(instruction)};
  }

  async refreshRecoveryBallot(params: {market: AddressLike; ballot: AddressLike}) {
    const market = address(params.market), ballot = address(params.ballot);
    const stored = await this.getRecoveryBallot(ballot);
    const target = recoveryTargetFromAnchor(stored.target);
    const state = await this.accountClient<Market>("market").fetch(market);
    const instruction = await this.writer.instruction("refreshRecoveryBallot" as DuskInstructionName, undefined, {accounts: {
      market, ballot, targetProposal: target.kind === "cancelProposal" ? address(target.proposal) : null,
      ylpMint: state.ylpMint, baseHlpYlpVault: state.baseHlpVault.ylpVault, quoteHlpYlpVault: state.quoteHlpVault.ylpVault,
    }});
    return {instruction, transaction: new Transaction().add(instruction)};
  }

  async releaseRecoveryVote(params: {market: AddressLike; ballot: AddressLike; source: AddressLike; round: GovernanceIntegerLike; rentRefund?: AddressLike}) {
    const market = address(params.market), ballot = address(params.ballot), source = address(params.source);
    const [vote] = deriveRecoveryVoteAddress(ballot, params.round, source, this.program.programId);
    const stored = await this.getRecoveryBallot(ballot);
    const target = recoveryTargetFromAnchor(stored.target);
    const receipt = params.rentRefund ? null : await this.accountClient<RecoveryVote>("recoveryVote").fetch(vote);
    const instruction = await this.writer.instruction("releaseRecoveryVote" as DuskInstructionName, undefined, {accounts: {
      market, ballot, source, vote, rentRefund: address(params.rentRefund ?? receipt!.rentRefund),
      targetProposal: target.kind === "cancelProposal" ? address(target.proposal) : null,
    }});
    return {instruction, transaction: new Transaction().add(instruction)};
  }

  async getBalance(market: AddressLike, owner: AddressLike, kind: GovernanceLpKind = "senior", lpMint?: AddressLike) {
    const marketKey = address(market);
    const state = await this.accountClient<Market>("market").fetch(marketKey);
    const [publicKey] = deriveGovernanceBalanceAddress(
      marketKey,
      address(owner),
      address(lpMint ?? governanceMint(state, kind)),
      this.program.programId
    );
    const account = await this.accountClient<GovernanceBalance>("governanceBalance").fetchNullable(publicKey);
    return account ? { publicKey, ...account, availableAmount: account.totalAmount.sub(account.committedAmount) } : null;
  }

  async getSupport(proposal: AddressLike, market: AddressLike, owner: AddressLike, kind: GovernanceLpKind = "senior", lpMint?: AddressLike) {
    const marketKey = address(market);
    const state = await this.accountClient<Market>("market").fetch(marketKey);
    const [balance] = deriveGovernanceBalanceAddress(
      marketKey,
      address(owner),
      address(lpMint ?? governanceMint(state, kind)),
      this.program.programId
    );
    const [publicKey] = deriveGovernanceSupportAddress(address(proposal), balance, this.program.programId);
    const account = await this.accountClient<GovernanceSupport>("governanceSupport").fetchNullable(publicKey);
    return account ? { publicKey, ...account } : null;
  }

  /** All allocations, including ended proposals awaiting release, for one owner. */
  async listSupport(market: AddressLike, owner: AddressLike, kind: GovernanceLpKind = "senior", lpMint?: AddressLike) {
    const marketKey = address(market);
    const state = await this.accountClient<Market>("market").fetch(marketKey);
    const [balance] = deriveGovernanceBalanceAddress(
      marketKey,
      address(owner),
      address(lpMint ?? governanceMint(state, kind)),
      this.program.programId
    );
    return this.accountClient<GovernanceSupport>("governanceSupport").all([{ memcmp: { offset: 40, bytes: balance.toBase58() } }]);
  }

  deposit(params: GovernanceOwnerParams & { amount: GovernanceIntegerLike }) {
    return this.manage(params, { deposit: { amount: positiveAmount(params.amount) } });
  }
  withdraw(params: GovernanceOwnerParams & { amount: GovernanceIntegerLike }) {
    return this.manage(params, { withdraw: { amount: positiveAmount(params.amount) } });
  }
  /** Only the owner signs. null revokes immediately without changing any allocations. */
  async setDelegate(params: { market: AddressLike; owner: AddressLike; delegate: AddressLike | null; kind?: GovernanceLpKind; lpMint?: AddressLike }) {
    const owner = address(params.owner);
    const market = address(params.market);
    const state = await this.accountClient<Market>("market").fetch(market);
    const [governanceBalance] = deriveGovernanceBalanceAddress(
      market,
      owner,
      address(params.lpMint ?? governanceMint(state, params.kind ?? "senior")),
      this.program.programId
    );
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
    const kind = params.kind ?? "senior";
    const lpMint = address(params.lpMint ?? governanceMint(state, kind));
    const [governanceBalance] = deriveGovernanceBalanceAddress(market, owner, lpMint, this.program.programId);
    const instruction = await this.writer.instruction("manageGovernanceBalance" as DuskInstructionName, action, { accounts: {
      owner, market, governanceBalance, lpMint,
      ownerLpAccount: address(params.ownerLpAccount ?? params.holderYlpAccount ?? getAssociatedTokenAddressSync(lpMint, owner, true, TOKEN_2022_PROGRAM_ID)),
      baseYieldAccount: address(params.baseYieldAccount ?? deriveYieldAccountAddress(market, owner, lpMint, state.baseSide.assetMint, kind === "senior" ? "ylp" : "junior", this.program.programId)[0]),
      quoteYieldAccount: address(params.quoteYieldAccount ?? deriveYieldAccountAddress(market, owner, lpMint, state.quoteSide.assetMint, kind === "senior" ? "ylp" : "junior", this.program.programId)[0]),
      token2022Program: TOKEN_2022_PROGRAM_ID, systemProgram: SystemProgram.programId,
    }, remainingAccounts: kind === "junior" && !lpMint.equals(address(state.juniorMint))
      ? [{ pubkey: deriveJuniorCohortAddress(market, lpMint, this.program.programId)[0], isSigner: false, isWritable: false }]
      : [] });
    return { governanceBalance, instruction, transaction: new Transaction().add(instruction) };
  }

  async createProposal(params: CreateGovernanceProposalParams) {
    assertProposalMetadata(params.metadata);
    const market = address(params.market), proposer = address(params.proposer);
    const [proposal] = deriveParameterProposalAddress(market, proposer, params.nonce, this.program.programId);
    const state = await this.accountClient<Market>("market").fetch(market);
    const batch = this.contributions(state, market, proposal, params.contributions);
    const instruction = await this.writer.instruction("createGovernanceProposal" as DuskInstructionName, {
      nonce: governanceIntegerBN(params.nonce, "nonce"),
      updates: canonicalGovernanceUpdates(params.updates, params.action).map(anchorParameterUpdate),
      action: anchorGovernanceAction(params.action), conditions: anchorRevisionConditions(params.conditions),
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
    const batch = this.contributions(state, market, proposal, params.contributions);
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
    const state = await this.accountClient<Market>("market").fetch(market);
    const [governanceBalance] = deriveGovernanceBalanceAddress(
      market,
      address(params.owner),
      address(params.lpMint ?? governanceMint(state, params.kind ?? "senior")),
      this.program.programId
    );
    const [governanceSupport] = deriveGovernanceSupportAddress(proposal, governanceBalance, this.program.programId);
    const proposer = params.proposer ?? (await this.accountClient<ParameterProposal>("parameterProposal").fetch(proposal)).proposer;
    const rentRefund = params.rentRefund ?? (await this.accountClient<GovernanceSupport>("governanceSupport").fetch(governanceSupport)).rentRefund;
    const instruction = await this.writer.instruction("withdrawGovernanceSupport" as DuskInstructionName,
      { amount: params.amount === undefined ? null : positiveAmount(params.amount) }, { accounts: {
        authority: address(params.authority), market, proposal, proposer: address(proposer), governanceBalance, governanceSupport, rentRefund: address(rentRefund),
      }});
    return { proposal, governanceBalance, governanceSupport, instruction, transaction: new Transaction().add(instruction) };
  }

  private contributions(state: Market, market: PublicKey, proposal: PublicKey, contributions: readonly GovernanceContribution[]) {
    if (!contributions.length) throw new Error("At least one governance contribution is required");
    const supports = contributions.map((contribution) => {
      const owner = address(contribution.owner);
      const [governanceBalance] = deriveGovernanceBalanceAddress(
        market,
        owner,
        governanceMint(state, contribution.kind ?? "senior"),
        this.program.programId
      );
      const [governanceSupport] = deriveGovernanceSupportAddress(proposal, governanceBalance, this.program.programId);
      return { owner, kind: contribution.kind ?? "senior", governanceBalance, governanceSupport, amount: positiveAmount(contribution.amount) };
    }).sort((a, b) => Buffer.compare(a.governanceBalance.toBuffer(), b.governanceBalance.toBuffer()));
    for (let i = 1; i < supports.length; i++) {
      if (supports[i].governanceBalance.equals(supports[i - 1].governanceBalance)) throw new Error("Duplicate governance balance");
    }
    const remainingAccounts: AccountMeta[] = supports.flatMap((s) => [s.governanceBalance, s.governanceSupport].map((pubkey) => ({ pubkey, isSigner: false, isWritable: true })));
    return { supports, amounts: supports.map((s) => s.amount), remainingAccounts };
  }
}

function governanceMint(state: Market, kind: GovernanceLpKind): PublicKey {
  return address(kind === "senior" ? state.ylpMint : state.juniorMint);
}

function positiveAmount(value: GovernanceIntegerLike) {
  const amount = governanceIntegerBN(value, "amount");
  if (amount.isZero()) throw new Error("Governance amount must be positive");
  return amount;
}
