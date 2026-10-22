import type { IdlTypes, Program } from "@coral-xyz/anchor";
import { createAssociatedTokenAccountIdempotentInstruction, getAssociatedTokenAddressSync, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { ComputeBudgetProgram, Transaction, type TransactionInstruction } from "@solana/web3.js";
import { address, type AddressLike } from "./address.js";
import { deriveFutarchyAuthorityAddress, deriveYieldAccountAddress } from "./constants.js";
import { governanceIntegerBN, type GovernanceIntegerLike } from "./governance.js";
import { DuskGet } from "./get.js";
import { previewReturnDataBytes } from "./preview.js";
import { tokenProgramForMint } from "./referral.js";
import type { Dusk as DuskIdl } from "./types_v2.js";
import { DuskWrite } from "./write.js";

export interface FundInterestParams {
  market: AddressLike;
  caller: AddressLike;
  /** Gross debt-token atoms, including the protocol allocation. Defaults to available cash. */
  maxBaseGross?: GovernanceIntegerLike;
  maxQuoteGross?: GovernanceIntegerLike;
}
export type FundInterestReceipt = IdlTypes<DuskIdl>["fundInterestReceipt"];
export interface HarvestYieldParams extends FundInterestParams {
  owner: AddressLike;
  kind: "ylp" | "baseHlp" | "quoteHlp";
  asset: "base" | "quote";
  /** Set false to collect existing funded claims independently of new funding. */
  fundAccruedInterest?: boolean;
}

/** Market-wide cash funding and holder-specific harvesting. Builders never
 * sign or submit. Funding can rebalance both hLPs and change the AMM mark;
 * moving interest out of reserves by itself does not change swap reserves.
 */
export class DuskYield {
  readonly write: DuskWrite;
  readonly get: DuskGet;
  constructor(readonly program: Program<DuskIdl>, feePayer?: AddressLike) {
    this.write = new DuskWrite(program);
    this.get = new DuskGet(program, feePayer);
  }

  async fundInstruction(params: FundInterestParams): Promise<TransactionInstruction> {
    const market = address(params.market);
    const s = await this.program.account.market.fetch(market);
    return this.write.instruction("fundInterest", {
      maxBaseGross: governanceIntegerBN(params.maxBaseGross ?? 0xffffffffffffffffn, "maxBaseGross"),
      maxQuoteGross: governanceIntegerBN(params.maxQuoteGross ?? 0xffffffffffffffffn, "maxQuoteGross"),
    }, { accounts: {
      market, caller: address(params.caller), futarchyAuthority: deriveFutarchyAuthorityAddress(this.program.programId)[0],
      baseMint: s.baseSide.assetMint, quoteMint: s.quoteSide.assetMint,
      baseReserveVault: s.baseSide.reserveVault, quoteReserveVault: s.quoteSide.reserveVault,
      baseInterestVault: s.baseSide.interestVault, quoteInterestVault: s.quoteSide.interestVault,
      tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID,
    }, remainingAccounts: await this.write.hlpRemainingAccounts(market, s) });
  }

  /** Simulates actual funding, transfer fees and the joint hLP rebalance.
   * This is a current-bank estimate, not a reservation of cash or yield.
   */
  async previewFunding(params: FundInterestParams): Promise<FundInterestReceipt> {
    const ix = await this.fundInstruction(params);
    const simulation = await this.get.simulateWithContext([
      ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
      ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }), ix,
    ]);
    const data = simulation.value.returnData;
    if (!data || data.programId !== this.program.programId.toBase58()) throw new Error("Missing Dusk funding return data");
    return this.program.coder.types.decode("fundInterestReceipt", previewReturnDataBytes(data)) as FundInterestReceipt;
  }

  async buildHarvest(params: HarvestYieldParams) {
    if (!["ylp", "baseHlp", "quoteHlp"].includes(params.kind)) throw new Error("Invalid yield token kind");
    if (params.asset !== "base" && params.asset !== "quote") throw new Error("Invalid yield asset");
    const market = address(params.market), owner = address(params.owner), caller = address(params.caller);
    const s = await this.program.account.market.fetch(market);
    const side = params.asset === "base" ? s.baseSide : s.quoteSide;
    const lpMint = params.kind === "ylp" ? s.ylpMint : params.kind === "baseHlp" ? s.baseSide.hlpMint : s.quoteSide.hlpMint;
    const kind = params.kind === "ylp" ? "ylp" : "hlp";
    const [yieldAccount] = deriveYieldAccountAddress(market, owner, lpMint, side.assetMint, kind, this.program.programId);
    const claim = await this.program.account.yieldAccount.fetch(yieldAccount);
    const tokenProgram = await tokenProgramForMint(this.program.provider.connection, side.assetMint);
    const recipientAssetAccount = getAssociatedTokenAddressSync(side.assetMint, claim.recipient, true, tokenProgram);
    const instructions: TransactionInstruction[] = [
      ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
      ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
      createAssociatedTokenAccountIdempotentInstruction(caller, recipientAssetAccount, claim.recipient, side.assetMint, tokenProgram),
    ];
    if (params.fundAccruedInterest !== false) instructions.push(await this.fundInstruction(params));
    instructions.push(await this.write.instruction("harvest", { tokenKind: { [kind]: {} } }, { accounts: {
      market, owner, caller, assetMint: side.assetMint, lpMint,
      ownerLpAccount: getAssociatedTokenAddressSync(lpMint, owner, true, TOKEN_2022_PROGRAM_ID),
      reserveVault: side.reserveVault, interestVault: side.interestVault, recipientAssetAccount, yieldAccount,
      tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID,
    } }));
    return { instructions, transaction: new Transaction().add(...instructions), yieldAccount, recipientAssetAccount };
  }
}
