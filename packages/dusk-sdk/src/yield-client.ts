import type { Program } from "@coral-xyz/anchor";
import { createAssociatedTokenAccountIdempotentInstruction, getAssociatedTokenAddressSync, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { ComputeBudgetProgram, Transaction, type AccountMeta, type TransactionInstruction } from "@solana/web3.js";
import { address, type AddressLike } from "./address.js";
import { deriveHlpCohortAddress, deriveJuniorCohortAddress, deriveYieldAccountAddress, yieldTokenKindArg } from "./constants.js";
import { DuskGet } from "./get.js";
import { tokenProgramForMint } from "./referral.js";
import type { Dusk as DuskIdl } from "./types_v2.js";
import { DuskWrite } from "./write.js";

export interface HarvestYieldParams {
  market: AddressLike;
  /** The LP holder, its designated recipient, or its harvest authority. */
  caller: AddressLike;
  owner: AddressLike;
  kind: "srlp" | "junior" | "baseHlp" | "quoteHlp";
  asset: "base" | "quote";
  /** Old hLP mint after terminal closure. Its yield indexes are frozen in its cohort PDA. */
  retiredHlpMint?: AddressLike;
  /** Wiped junior generation; interest credited before the wipe remains claimable. */
  retiredJuniorMint?: AddressLike;
  /** Extra accounts required by a Token-2022 asset transfer hook. */
  transferHookAccounts?: readonly AccountMeta[];
}

/** Holder-specific harvesting. Builders never sign or submit. Interest is
 * credited to holders when it accrues; a harvest pays swap fees in full and
 * credited interest from reserve cash as it allows, leaving the rest
 * claimable. Harvesting does not change swap reserves.
 */
export class DuskYield {
  readonly write: DuskWrite;
  readonly get: DuskGet;
  constructor(readonly program: Program<DuskIdl>, feePayer?: AddressLike) {
    this.write = new DuskWrite(program);
    this.get = new DuskGet(program, feePayer);
  }

  async buildHarvest(params: HarvestYieldParams) {
    if (!["srlp", "junior", "baseHlp", "quoteHlp"].includes(params.kind)) throw new Error("Invalid yield token kind");
    if (params.asset !== "base" && params.asset !== "quote") throw new Error("Invalid yield asset");
    const market = address(params.market), owner = address(params.owner), caller = address(params.caller);
    const s = await this.get.market(market);
    const side = params.asset === "base" ? s.baseSide : s.quoteSide;
    if (params.kind === "srlp" && params.retiredHlpMint) throw new Error("srLP does not have receipt cohorts");
    if (params.retiredJuniorMint && params.kind !== "junior") throw new Error("A retired junior mint requires junior kind");
    if (params.kind === "junior" && params.retiredHlpMint) throw new Error("Junior cannot use an hLP cohort");
    const lpMint = params.retiredJuniorMint ? address(params.retiredJuniorMint) : params.retiredHlpMint
      ? address(params.retiredHlpMint)
      : params.kind === "srlp" ? s.srlpMint : params.kind === "junior" ? s.juniorMint : params.kind === "baseHlp" ? s.baseSide.hlpMint : s.quoteSide.hlpMint;
    const kind = params.kind === "srlp" ? "srlp" : params.kind === "junior" ? "junior" : "hlp";
    const [yieldAccount] = deriveYieldAccountAddress(market, owner, lpMint, side.assetMint, kind, this.program.programId);
    const claim = await this.get.yieldAccount(yieldAccount);
    const tokenProgram = await tokenProgramForMint(this.program.provider.connection, side.assetMint);
    const recipientAssetAccount = getAssociatedTokenAddressSync(side.assetMint, claim.recipient, true, tokenProgram);
    const instructions: TransactionInstruction[] = [
      ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
      ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
      createAssociatedTokenAccountIdempotentInstruction(caller, recipientAssetAccount, claim.recipient, side.assetMint, tokenProgram),
    ];
    instructions.push(await this.write.instruction("harvest", { tokenKind: yieldTokenKindArg(kind) }, { accounts: {
      market, owner, caller, assetMint: side.assetMint, lpMint,
      ownerLpAccount: getAssociatedTokenAddressSync(lpMint, owner, true, TOKEN_2022_PROGRAM_ID),
      reserveVault: side.reserveVault, recipientAssetAccount, yieldAccount,
      tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID,
    }, remainingAccounts: [
      ...(params.retiredHlpMint
        ? [{ pubkey: deriveHlpCohortAddress(market, lpMint, this.program.programId)[0], isSigner: false, isWritable: false }]
        : []),
      ...(params.retiredJuniorMint
        ? [{ pubkey: deriveJuniorCohortAddress(market, lpMint, this.program.programId)[0], isSigner: false, isWritable: false }]
        : []),
      ...(params.transferHookAccounts ?? []),
    ],
    }));
    return { instructions, transaction: new Transaction().add(...instructions), yieldAccount, recipientAssetAccount };
  }
}
