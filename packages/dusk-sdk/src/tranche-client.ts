import type { BN, Program } from "@coral-xyz/anchor";
import { PublicKey, SystemProgram, Transaction, SYSVAR_INSTRUCTIONS_PUBKEY, type AccountMeta } from "@solana/web3.js";
import { createAssociatedTokenAccountIdempotentInstruction, getAssociatedTokenAddressSync, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { address, type AddressLike } from "./address.js";
import { deriveJuniorMintAddress, deriveJuniorCohortAddress, deriveJuniorExitAddress, deriveYieldAccountAddress, deriveIncomeEpochAddress, deriveTokenMetadataAddress, TOKEN_METADATA_PROGRAM_ID } from "./constants.js";
import { integerBN, type IntegerLike } from "./parameters.js";
import { assertLpTokenMetadata, type LpTokenMetadataInput } from "./lp-naming.js";
import { oracleComputeBudgetInstructions, oracleReadAccounts } from "./oracle.js";
import { DuskWrite } from "./write.js";
import { assertCurrentMarketLayout } from "./market-layout.js";
import type { Dusk as DuskIdl } from "./types_v2.js";
import type { Market } from "./type-aliases.js";

type IncomeBook = Market["income"]["books"][number];

/** Whether `value` reaches half of an unsigned `bits`-bit range (Rust's
 * `MAX / 2`, which is 2^(bits−1) − 1). */
const atHalfRange = (value: BN, bits: number) => value.addn(1).bitLength() >= bits;

/** Mirrors `roll_income_epoch`: a book with settled units rolls once at least
 * half its units are repaid, or once any counter reaches half its range.
 * Rolling earlier would let anyone fragment holders' catch-up into many
 * archived epochs. */
export function incomeBookMayRoll(book: IncomeBook): boolean {
  if (book.units.lte(book.unpaid)) return false;
  if (book.unpaid.lte(book.units.divn(2))) return true;
  return (
    atHalfRange(book.units, 64) ||
    atHalfRange(book.unpaid, 64) ||
    atHalfRange(book.paidPerUnitQ64, 128) ||
    book.growth.some((lane) => atHalfRange(lane.unitsQ64, 128) || atHalfRange(lane.weightedPaidQ128[3], 64))
  );
}

export type TrancheAction =
  | { kind: "enter"; seniorAmount: IntegerLike; minimumJunior: IntegerLike }
  | { kind: "requestExit"; juniorAmount: IntegerLike }
  | { kind: "convertExit"; minimumSenior: IntegerLike }
  | { kind: "cancelExit" };

/** srLP/jrLP builders. Conversion changes ownership of existing pool capital;
 * redeeming cash is a separate ordinary-LP withdrawal. Builders never submit. */
export class DuskTranches {
  readonly write: DuskWrite;
  constructor(readonly program: Program<DuskIdl>) { this.write = new DuskWrite(program); }

  // Throws DuskUnsupportedMarketLayoutError for a market at any other layout.
  private async marketState(market: PublicKey) {
    return assertCurrentMarketLayout(await this.program.account.market.fetch(market));
  }

  /** Archive an income denomination once at least half of its units are
   * repaid, or once it nears its numeric limits, so further issuance does not
   * overflow. Anyone may roll; this does not claim anyone's income. */
  async rollIncome(params: { market: AddressLike; payer: AddressLike; bookIndex: number }) {
    const market = address(params.market), payer = address(params.payer);
    const state = await this.marketState(market);
    const { bookIndex } = params;
    if (!Number.isInteger(bookIndex) || bookIndex < 0 || bookIndex > 3) throw new Error("bookIndex must be 0–3");
    const book = state.income.books[bookIndex];
    if (!incomeBookMayRoll(book)) {
      throw new Error("Income book cannot roll until half its units are repaid or it nears its numeric limits");
    }
    if (!state.income.pending[bookIndex].isZero()) throw new Error("Income custody settlement is pending");
    const incomeEpoch = deriveIncomeEpochAddress(market, bookIndex, book.epoch, this.program.programId)[0];
    const instruction = await this.write.instruction("rollIncomeEpoch", { bookIndex, epoch: book.epoch }, { accounts: {
      payer, market, incomeEpoch, systemProgram: SystemProgram.programId,
    } });
    return { incomeEpoch, instruction, transaction: new Transaction().add(instruction) };
  }

  /** Prepare one bounded history catch-up per source. Submit and fetch again
   * while hasMore is true, before transferring, converting or harvesting. */
  async checkpointIncome(params: { market: AddressLike; owner: AddressLike; kind: "senior" | "junior"; asset: "base" | "quote"; retiredJuniorMint?: AddressLike }) {
    const market = address(params.market), owner = address(params.owner);
    if (params.kind !== "junior" && params.retiredJuniorMint) throw new Error("Only junior has retired generations");
    const state = await this.marketState(market);
    const lpMint = params.kind === "senior" ? state.srlpMint : params.retiredJuniorMint ? address(params.retiredJuniorMint) : state.juniorMint;
    const assetIndex = params.asset === "base" ? 0 : 1;
    const assetMint = assetIndex === 0 ? state.baseSide.assetMint : state.quoteSide.assetMint;
    const yieldAccount = deriveYieldAccountAddress(market, owner, lpMint, assetMint, params.kind === "senior" ? "srlp" : "junior", this.program.programId)[0];
    const holder = await this.program.account.yieldAccount.fetch(yieldAccount);
    if (!holder.incomeInitialized) throw new Error("Initialize the holder's income ledger before catch-up");
    const ownerLp = getAssociatedTokenAddressSync(lpMint, owner, true, TOKEN_2022_PROGRAM_ID);
    const instructions = [];
    let hasMore = false;
    for (let source = 0; source < 2; source++) {
      const bookIndex = assetIndex + source * 2;
      const epoch = BigInt(holder.income[source].epoch.toString());
      const liveEpoch = BigInt(state.income.books[bookIndex].epoch.toString());
      if (epoch > liveEpoch) throw new Error("Holder income epoch exceeds market epoch");
      const count = Number(liveEpoch - epoch > 8n ? 8n : liveEpoch - epoch);
      const remainingAccounts: AccountMeta[] = [];
      if (params.kind === "junior" && !lpMint.equals(state.juniorMint)) remainingAccounts.push({ pubkey: deriveJuniorCohortAddress(market, lpMint, this.program.programId)[0], isSigner: false, isWritable: false });
      for (let i = 0; i < count; i++) remainingAccounts.push({ pubkey: deriveIncomeEpochAddress(market, bookIndex, epoch + BigInt(i), this.program.programId)[0], isSigner: false, isWritable: false });
      instructions.push(await this.write.instruction("checkpointIncome", { source }, { accounts: { market, lpMint, ownerLp, yieldAccount }, remainingAccounts }));
      hasMore ||= epoch + BigInt(count) < liveEpoch;
    }
    return { instructions, hasMore, yieldAccount };
  }

  /** Prepare bounded, separate transactions to catch up both assets before a
   * srLP/jrLP transfer, exit or claim. No transaction is sent by this method.
   * If hasMore is true, submit these batches and call again. A concurrent epoch
   * roll can also require another fetch; never discard the holder's old claim. */
  async prepareHolderIncome(params: { market: AddressLike; owner: AddressLike; kind: "senior" | "junior"; retiredJuniorMint?: AddressLike }) {
    const transactions: Transaction[] = [];
    let hasMore = false;
    for (const asset of ["base", "quote"] as const) {
      const batch = await this.checkpointIncome({ ...params, asset });
      transactions.push(...batch.instructions.map(ix => new Transaction().add(ix)));
      hasMore ||= batch.hasMore;
    }
    return { transactions, hasMore };
  }

  /** Archive drained books before another operation issues income. This is a
   * market-wide maintenance step, separate from catching up individual holders.
   * A numerically exhausted, partially paid book can be rolled explicitly with
   * rollIncome after IncomeEpochNeedsRollover. The payer funds archival rent. */
  async prepareDrainedIncomeBooks(params: { market: AddressLike; payer: AddressLike }) {
    const state = await this.marketState(address(params.market));
    const transactions: Transaction[] = [];
    for (let bookIndex = 0; bookIndex < 4; bookIndex++) {
      const book = state.income.books[bookIndex];
      if (!book.units.isZero() && book.unpaid.isZero()) {
        transactions.push((await this.rollIncome({ ...params, bookIndex })).transaction);
      }
    }
    return { transactions };
  }

  /** A retired receipt has zero principal but can still own earned income. */
  async burnRetired(params: { market: AddressLike; owner: AddressLike; retiredMint: AddressLike; amount: IntegerLike }) {
    const market = address(params.market), owner = address(params.owner), retiredMint = address(params.retiredMint);
    const state = await this.marketState(market);
    if (retiredMint.equals(state.juniorMint)) throw new Error("Cannot burn an active generation through the retired-receipt path");
    const exitAddress = deriveJuniorExitAddress(market, owner, retiredMint, this.program.programId)[0];
    const exitState = await this.program.account.juniorExitAccount.fetchNullable(exitAddress);
    const instruction = await this.write.instruction("burnRetiredJunior", { amount: integerBN(params.amount, "amount") }, { accounts: {
      market, owner, retiredMint, cohort: deriveJuniorCohortAddress(market, retiredMint, this.program.programId)[0],
      ownerJunior: getAssociatedTokenAddressSync(retiredMint, owner, true, TOKEN_2022_PROGRAM_ID),
      baseYield: deriveYieldAccountAddress(market, owner, retiredMint, state.baseSide.assetMint, "junior", this.program.programId)[0],
      quoteYield: deriveYieldAccountAddress(market, owner, retiredMint, state.quoteSide.assetMint, "junior", this.program.programId)[0],
      exit: exitState ? exitAddress : null, token2022Program: TOKEN_2022_PROGRAM_ID,
    } });
    return { instruction, transaction: new Transaction().add(instruction) };
  }

  async fundSeed(params: { market: AddressLike; asset: "base" | "quote"; remainingAccounts?: AccountMeta[] }) {
    const market = address(params.market);
    const state = await this.marketState(market);
    const side = params.asset === "base" ? state.baseSide : state.quoteSide;
    const instruction = await this.write.instruction("fundJuniorSeed", undefined, { accounts: {
      market, assetMint: side.assetMint, reserveVault: side.reserveVault, interestVault: side.interestVault,
      tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID,
    }, remainingAccounts: params.remainingAccounts });
    return { instruction, transaction: new Transaction().add(instruction) };
  }

  async compoundSeed(params: { market: AddressLike }) {
    const market = address(params.market);
    const state = await this.marketState(market);
    const policy = state.oracle.policy;
    const instruction = await this.write.instruction("compoundJuniorSeed", undefined, {
      accounts: { market, juniorMint: state.juniorMint },
      remainingAccounts: oracleReadAccounts(policy),
    });
    return {
      instruction,
      transaction: new Transaction().add(...oracleComputeBudgetInstructions(policy), instruction),
    };
  }

  async initialize(params: { market: AddressLike; payer: AddressLike; metadata: LpTokenMetadataInput }) {
    const market = address(params.market), payer = address(params.payer);
    assertLpTokenMetadata(params.metadata);
    const state = await this.marketState(market);
    const first = state.juniorMint.equals(PublicKey.default);
    if (!first && !state.tranches.juniorReserveShares.isZero()) throw new Error("Current junior capital is not wiped out");
    const generation = BigInt(state.tranches.generation.toString()) + (first ? 0n : 1n);
    const juniorMint = deriveJuniorMintAddress(market, generation, this.program.programId)[0];
    const cohort = deriveJuniorCohortAddress(market, juniorMint, this.program.programId)[0];
    const initialize = await this.write.instruction("initializeJunior", {
      generation: integerBN(generation, "generation"),
      metadata: params.metadata,
    }, { accounts: {
      payer, market, juniorMint, cohort,
      tokenMetadata: deriveTokenMetadataAddress(juniorMint)[0], sysvarInstructions: SYSVAR_INSTRUCTIONS_PUBKEY, tokenMetadataProgram: TOKEN_METADATA_PROGRAM_ID,
      previousCohort: first ? null : deriveJuniorCohortAddress(market, state.juniorMint, this.program.programId)[0],
      token2022Program: TOKEN_2022_PROGRAM_ID, systemProgram: SystemProgram.programId,
    } });
    const hook = await this.write.initializeLpTransferHookInstruction({ payer, market, lpMint: juniorMint, tokenKind: "junior" });
    return { juniorMint, cohort, generation, instructions: [initialize, hook], transaction: new Transaction().add(initialize, hook) };
  }

  async convert(params: { market: AddressLike; owner: AddressLike; action: TrancheAction }) {
    const market = address(params.market), owner = address(params.owner);
    const state = await this.marketState(market);
    if (state.juniorMint.equals(PublicKey.default)) throw new Error("Initialize the junior mint first");
    const seniorMint = state.srlpMint, juniorMint = state.juniorMint;
    const ownerSenior = getAssociatedTokenAddressSync(seniorMint, owner, true, TOKEN_2022_PROGRAM_ID);
    const ownerJunior = getAssociatedTokenAddressSync(juniorMint, owner, true, TOKEN_2022_PROGRAM_ID);
    const exit = deriveJuniorExitAddress(market, owner, juniorMint, this.program.programId)[0];
    const action = params.action.kind === "enter"
      ? { enter: { seniorAmount: integerBN(params.action.seniorAmount, "seniorAmount"), minimumJunior: integerBN(params.action.minimumJunior, "minimumJunior") } }
      : params.action.kind === "requestExit"
        ? { requestExit: { juniorAmount: integerBN(params.action.juniorAmount, "juniorAmount") } }
        : params.action.kind === "convertExit"
          ? { convertExit: { minimumSenior: integerBN(params.action.minimumSenior, "minimumSenior") } }
          : { cancelExit: {} };
    const ix = await this.write.instruction("convertTranche", action, { accounts: {
      owner, market, seniorMint, juniorMint, ownerSenior, ownerJunior, exit,
      seniorBaseYield: deriveYieldAccountAddress(market, owner, seniorMint, state.baseSide.assetMint, "srlp", this.program.programId)[0],
      seniorQuoteYield: deriveYieldAccountAddress(market, owner, seniorMint, state.quoteSide.assetMint, "srlp", this.program.programId)[0],
      juniorBaseYield: deriveYieldAccountAddress(market, owner, juniorMint, state.baseSide.assetMint, "junior", this.program.programId)[0],
      juniorQuoteYield: deriveYieldAccountAddress(market, owner, juniorMint, state.quoteSide.assetMint, "junior", this.program.programId)[0],
      token2022Program: TOKEN_2022_PROGRAM_ID, systemProgram: SystemProgram.programId,
    } });
    const instructions = [
      createAssociatedTokenAccountIdempotentInstruction(owner, ownerSenior, owner, seniorMint, TOKEN_2022_PROGRAM_ID),
      createAssociatedTokenAccountIdempotentInstruction(owner, ownerJunior, owner, juniorMint, TOKEN_2022_PROGRAM_ID), ix,
    ];
    return { instructions, transaction: new Transaction().add(...instructions), exit, seniorMint, juniorMint };
  }
}
