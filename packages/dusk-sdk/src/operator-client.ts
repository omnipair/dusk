import type { Program } from "@coral-xyz/anchor";
import { ComputeBudgetProgram, SystemProgram, Transaction, type AccountMeta, type PublicKey } from "@solana/web3.js";
import { address, type AddressLike } from "./address.js";
import { deriveOperatorProposalAddress, deriveOperatorRevenueAddress, OPERATOR_POLICY_VERSION } from "./constants.js";
import { sha256 } from "./hash.js";
import {
  anchorParameterUpdate, assertProposalMetadata, canonicalParameterUpdates, integerBN,
  type IntegerLike, type ParameterUpdate, type ProposalMetadataV1,
} from "./parameters.js";
import type { Market, OperatorProposal, OperatorRevenueAccount } from "./type-aliases.js";
import type { Dusk } from "./types_v2.js";
import { DuskWrite } from "./write.js";
import { createAssociatedTokenAccountIdempotentInstruction, getAssociatedTokenAddressSync, TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, getMint } from "@solana/spl-token";
import { tokenProgramForMint, resolveTransferHookAccountMetas } from "./referral.js";

export interface AnnounceOperatorChangeParams {
  payer: AddressLike;
  operator: AddressLike;
  market: AddressLike;
  updates?: readonly ParameterUpdate[];
  nextOperator?: AddressLike;
  /** Optional parameter delegate. The default public key clears it. */
  nextManager?: AddressLike;
  revenueRecipient?: AddressLike;
  interestShareBps?: number;
  metadata: ProposalMetadataV1;
  /** Optional concurrency commitments; defaults are fetched from the market. */
  nonce?: IntegerLike;
  expectedEpoch?: IntegerLike;
}

/** Core authority only. Token voting, elections and distribution live in the
 * external program that holds the operator signer PDA. Payers are independent. */
export class DuskOperator {
  readonly write: DuskWrite;
  constructor(readonly program: Program<Dusk>) { this.write = new DuskWrite(program); }

  market(key: AddressLike): Promise<Market> {
    return this.program.account.market.fetch(address(key));
  }
  proposal(key: AddressLike): Promise<OperatorProposal> {
    return this.program.account.operatorProposal.fetch(address(key));
  }
  async pending(market: AddressLike): Promise<{ address: PublicKey; proposal: OperatorProposal } | null> {
    const state = await this.market(market);
    if (state.operator.pending.equals(SystemProgram.programId)) return null;
    return { address: state.operator.pending, proposal: await this.proposal(state.operator.pending) };
  }
  async announce(params: AnnounceOperatorChangeParams) {
    assertProposalMetadata(params.metadata);
    if (params.nextOperator !== undefined && params.nextManager !== undefined) {
      throw new Error("an operator transfer clears the manager; appoint a manager separately");
    }
    const updates = params.updates?.length ? canonicalParameterUpdates(params.updates) : [];
    if (!updates.length && params.nextOperator === undefined && params.nextManager === undefined && params.revenueRecipient === undefined && params.interestShareBps === undefined) {
      throw new Error("an operator bundle must change at least one setting");
    }
    if (params.interestShareBps !== undefined && (!Number.isInteger(params.interestShareBps) || params.interestShareBps < 0 || params.interestShareBps > 5_000)) {
      throw new Error("operator interest share must be 0–5000 bps");
    }
    const market = address(params.market);
    const state = await this.market(market);
    const nonce = integerBN(params.nonce ?? state.operator.nextNonce, "nonce");
    const [proposal] = deriveOperatorProposalAddress(market, nonce, this.program.programId);
    const instruction = await this.write.instruction("announceOperatorChange", {
      nonce, expectedEpoch: integerBN(params.expectedEpoch ?? state.operator.epoch, "expectedEpoch"),
      updates: updates.map(anchorParameterUpdate), nextOperator: params.nextOperator === undefined ? null : address(params.nextOperator),
      nextManager: params.nextManager === undefined ? null : address(params.nextManager),
      revenueRecipient: params.revenueRecipient === undefined ? null : address(params.revenueRecipient),
      interestShareBps: params.interestShareBps ?? null, metadata: params.metadata,
    }, { accounts: {payer: address(params.payer), operator: address(params.operator), market, proposal, systemProgram: SystemProgram.programId} });
    return { proposal, instruction, transaction: new Transaction().add(instruction) };
  }
  async execute(params: {market: AddressLike; proposal: AddressLike; payer?:AddressLike; acceptingOperator?: AddressLike; remainingAccounts?: AccountMeta[]}) {
    const state = await this.market(params.market);
    const proposal = await this.proposal(params.proposal);
    const next = proposal.revenueRecipient ?? (proposal.nextOperator && !proposal.nextOperator.equals(SystemProgram.programId)
      ? proposal.nextOperator : state.operator.revenueRecipient);
    let previousRevenue:PublicKey|null=null;
    const setupInstructions=[];
    if (!next.equals(state.operator.revenueRecipient)) {
      previousRevenue=deriveOperatorRevenueAddress(address(params.market),state.operator.revenueEpoch,this.program.programId)[0];
      if(!await this.program.provider.connection.getAccountInfo(previousRevenue)) {
        const payer=params.payer ?? (this.program.provider as {publicKey?:PublicKey}).publicKey;
        if(!payer)throw new Error("a funded payer is required to preserve the previous recipient's revenue");
        setupInstructions.push(await this.initializeRevenueInstruction({market:params.market,payer}));
      }
    }
    const builder = this.write.builder("executeOperatorChange", undefined, {
      oraclePolicy: state.oracle?.policy,
      accounts: {market: address(params.market), proposal: address(params.proposal), previousRevenue,
        acceptingOperator: params.acceptingOperator === undefined ? null : address(params.acceptingOperator)},
      remainingAccounts: params.remainingAccounts,
    });
    const instruction = await builder.instruction();
    const transaction = await builder.transaction();
    transaction.instructions.unshift(...setupInstructions);
    if (!state.oracle?.policy.lambdaBps) transaction.instructions.unshift(ComputeBudgetProgram.setComputeUnitLimit({units:1_400_000}));
    return { instruction, setupInstructions, transaction };
  }
  async removeManager(params: {market: AddressLike; operator: AddressLike}) {
    const instruction = await this.write.instruction("removeMarketManager", undefined,
      {accounts: {market: address(params.market), operator: address(params.operator)}});
    return {instruction, transaction: new Transaction().add(instruction)};
  }
  async initializeRevenueInstruction(params:{market:AddressLike;payer:AddressLike}) {
    const state=await this.market(params.market);
    const [revenue]=deriveOperatorRevenueAddress(address(params.market),state.operator.revenueEpoch,this.program.programId);
    return this.write.instruction("initializeOperatorRevenue",undefined,{accounts:{market:address(params.market),payer:address(params.payer),revenue,systemProgram:SystemProgram.programId}});
  }
  revenue(key:AddressLike):Promise<OperatorRevenueAccount> {
    return this.program.account.operatorRevenueAccount.fetch(address(key));
  }

  async claim(params:{market:AddressLike;revenueEpoch?:IntegerLike;assetMint:AddressLike;payer:AddressLike;maxDebit?:IntegerLike;minimumCredit?:IntegerLike;remainingAccounts?:AccountMeta[]}) {
    const market=address(params.market),payer=address(params.payer),mint=address(params.assetMint);
    const state=await this.market(market);
    const epoch=integerBN(params.revenueEpoch??state.operator.revenueEpoch,"revenueEpoch");
    const current=epoch.eq(state.operator.revenueEpoch);
    const revenue=current?null:deriveOperatorRevenueAddress(market,epoch,this.program.programId)[0];
    const archive=revenue?await this.revenue(revenue):null;
    const recipient=current?state.operator.revenueRecipient:archive!.recipient;
    const side=state.baseSide.assetMint.equals(mint)?state.baseSide:state.quoteSide.assetMint.equals(mint)?state.quoteSide:null;
    if(!side)throw new Error("asset mint is not part of the market");
    const asset=state.baseSide.assetMint.equals(mint)?0:1;
    let claimable=0n;
    for(const i of [asset,asset+2]) {
      const revenueBook=state.income.revenue[i]!;
      claimable+=BigInt((current?revenueBook.operatorClaimable:archive!.claimable[i]!).toString());
    }
    const requested=BigInt(integerBN(params.maxDebit??0xffff_ffff_ffff_ffffn,"maxDebit").toString());
    const debit=claimable<requested?claimable:requested;
    if(debit===0n)throw new Error("no collected operator cash is claimable");
    const tokenProgram=await tokenProgramForMint(this.program.provider.connection,mint);
    const mintState=await getMint(this.program.provider.connection,mint,undefined,tokenProgram);
    const ata=getAssociatedTokenAddressSync(mint,recipient,true,tokenProgram);
    const hook=await resolveTransferHookAccountMetas(this.program.provider.connection,[{
      mint,source:side.interestVault,destination:ata,authority:market,amount:debit,decimals:mintState.decimals,tokenProgram,
    }]);
    const instruction=await this.write.instruction("claimOperatorRevenue",{revenueEpoch:epoch,
      maxDebit:integerBN(debit,"maxDebit"),minimumCredit:integerBN(params.minimumCredit??0,"minimumCredit")},
      {accounts:{market,revenue,recipient,assetMint:mint,interestVault:side.interestVault,recipientAssetAccount:ata,
        tokenProgram:TOKEN_PROGRAM_ID,token2022Program:TOKEN_2022_PROGRAM_ID},remainingAccounts:[...hook,...(params.remainingAccounts??[])]});
    const transaction=new Transaction().add(createAssociatedTokenAccountIdempotentInstruction(payer,ata,recipient,mint,tokenProgram),instruction);
    return {instruction,transaction,recipient,revenue,recipientAssetAccount:ata};
  }
  async resolve(params: {market: AddressLike; proposal: AddressLike; operator?: AddressLike; cancel?: boolean}) {
    const proposal = await this.proposal(params.proposal);
    if (!proposal.market.equals(address(params.market))) throw new Error("proposal belongs to another market");
    const instruction = await this.write.instruction("resolveOperatorChange", params.cancel ?? false, {
      accounts: {market: address(params.market), proposal: address(params.proposal), rentRefund: proposal.rentRefund,
        operator: params.operator === undefined ? null : address(params.operator)},
    });
    return { instruction, transaction: new Transaction().add(instruction) };
  }
  cancel(params: {market: AddressLike; proposal: AddressLike; operator: AddressLike}) {
    return this.resolve({...params, cancel: true});
  }

  /** Verify the on-chain notice commitment using the generated Borsh layout.
   * Immutable fields precede the trailing digest (32), executed (1), bump (1). */
  async verifyDigest(proposal: OperatorProposal): Promise<boolean> {
    if (proposal.policyVersion.toString() !== OPERATOR_POLICY_VERSION.toString()) return false;
    const bytes = await this.program.coder.accounts.encode("operatorProposal", proposal);
    const domain = Buffer.from("DUSK_OPERATOR_CHANGE_V2");
    const digest = await sha256(Buffer.concat([domain, this.program.programId.toBuffer(), bytes.subarray(8, bytes.length - 34)]));
    return digest.every((value, index) => value === proposal.digest[index]);
  }
}
